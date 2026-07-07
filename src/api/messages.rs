//! Anthropic Messages API compatibility.

use axum::{
    extract::State,
    response::{IntoResponse, Response, Sse},
    Json,
};
use futures::StreamExt;
use serde::Deserialize;
use std::convert::Infallible;

use super::tools::{
    has_trusted_tool_prompt, looks_like_tool_call, looks_like_tool_prompt, looks_like_tool_refusal,
    normalize_tools_for_prompt, parse_tool_uses, tool_choice_to_prompt_value, tools_prompt,
    ToolModeStreamBuffer,
};
use crate::account_pool::AccountPool;
use crate::models::resolve_model;
use crate::pool::acquire_direct_permit;
use crate::providers::{
    complete_completion, proxy_url_for_model, requires_use_ai_account, stream_completion,
    CompletionRequest,
};

// Thinking level to budget (same as chat.rs)
const THINKING_LEVELS: &[(&str, usize)] = &[
    ("low", 1024),
    ("medium", 5000),
    ("high", 16000),
    ("max", 32000),
];
#[derive(Debug, Deserialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub system: Option<serde_json::Value>,
    #[serde(default)]
    pub max_tokens: Option<usize>,
    #[serde(default)]
    pub thinking: Option<ThinkingParam>,
    #[serde(default)]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ThinkingParam {
    Bool(bool),
    Level(String),
    Object {
        #[serde(rename = "type")]
        type_: String,
        #[serde(default)]
        budget_tokens: Option<usize>,
    },
}

pub fn routes() -> axum::Router<AccountPool> {
    axum::Router::new().route("/messages", axum::routing::post(handler))
}

fn anthropic_session_id(req: &AnthropicRequest) -> String {
    req.metadata
        .as_ref()
        .and_then(|m| m.get("session_id").or_else(|| m.get("user_id")))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string()
}

fn summarize_anthropic_messages(messages: &[serde_json::Value]) -> String {
    messages
        .iter()
        .enumerate()
        .map(|(idx, msg)| {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("?");
            let content = msg
                .get("content")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let preview = content.to_string().chars().take(80).collect::<String>();
            format!("{}:{}:{}", idx, role, preview)
        })
        .collect::<Vec<_>>()
        .join(" | ")
}
fn convert_anthropic_content(content: Option<&serde_json::Value>) -> serde_json::Value {
    match content {
        Some(serde_json::Value::String(s)) => serde_json::Value::String(s.clone()),
        Some(serde_json::Value::Array(arr)) => {
            let parts = arr
                .iter()
                .filter_map(|item| match item.get("type").and_then(|v| v.as_str()) {
                    Some("text") => item.get("text").and_then(|v| v.as_str()).map(|text| {
                        serde_json::json!({
                            "type": "text",
                            "text": text,
                        })
                    }),
                    Some("image") => {
                        let source = item.get("source")?;
                        let media_type = source
                            .get("media_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or("image/png");
                        let data = source.get("data").and_then(|v| v.as_str())?;
                        Some(serde_json::json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{};base64,{}", media_type, data),
                            },
                            "filename": "image.png",
                        }))
                    }
                    Some("document") | Some("file") => {
                        let filename = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .or_else(|| item.get("filename").and_then(|v| v.as_str()))
                            .unwrap_or("file");
                        if let Some(source) = item.get("source").and_then(|v| v.as_object()) {
                            let media_type = source
                                .get("media_type")
                                .and_then(|v| v.as_str())
                                .or_else(|| item.get("media_type").and_then(|v| v.as_str()))
                                .unwrap_or("application/octet-stream");
                            if let Some(data) = source.get("data").and_then(|v| v.as_str()) {
                                Some(serde_json::json!({
                                    "type": "file",
                                    "file": {
                                        "data": format!("data:{};base64,{}", media_type, data),
                                        "filename": filename,
                                        "media_type": media_type,
                                    }
                                }))
                            } else {
                                source.get("url").and_then(|v| v.as_str()).map(|url| {
                                    serde_json::json!({
                                        "type": "file",
                                        "file": {
                                            "url": url,
                                            "filename": filename,
                                            "media_type": media_type,
                                        }
                                    })
                                })
                            }
                        } else {
                            item.get("url").and_then(|v| v.as_str()).map(|url| {
                                serde_json::json!({
                                    "type": "file",
                                    "file": {
                                        "url": url,
                                        "filename": filename,
                                        "media_type": item
                                            .get("media_type")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("application/octet-stream"),
                                    }
                                })
                            })
                        }
                    }
                    Some("tool_result") => {
                        let tool_use_id = item
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let result_content = item
                            .get("content")
                            .map(|v| {
                                v.as_str()
                                    .map(ToOwned::to_owned)
                                    .unwrap_or_else(|| v.to_string())
                            })
                            .unwrap_or_default();
                        Some(serde_json::json!({
                            "type": "text",
                            "text": format!(
                                "Tool result for {} has completed. Continue the user's task using this result:\n{}",
                                tool_use_id,
                                result_content
                            ),
                        }))
                    }
                    Some("tool_use") => {
                        let name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let input = item
                            .get("input")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({}));
                        Some(serde_json::json!({
                            "type": "text",
                            "text": format!(
                                "<tool_use>\n{}\n</tool_use>",
                                serde_json::json!({
                                    "name": name,
                                    "input": input
                                })
                            ),
                        }))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            serde_json::Value::Array(parts)
        }
        Some(other) => other.clone(),
        None => serde_json::Value::String(String::new()),
    }
}

fn truncate_to_token_budget(text: String, max_tokens: Option<usize>) -> String {
    let Some(max_tokens) = max_tokens else {
        return text;
    };
    let max_chars = max_tokens.saturating_mul(4);
    if text.len() <= max_chars {
        return text;
    }

    let mut end = 0;
    for (idx, _) in text.char_indices() {
        if idx > max_chars {
            break;
        }
        end = idx;
    }
    text[..end].to_string()
}

async fn handler(State(pool): State<AccountPool>, Json(req): Json<AnthropicRequest>) -> Response {
    let _permit = match acquire_direct_permit().await {
        Ok(p) => p,
        Err(e) => {
            return Json(serde_json::json!({
                "error": format!("Concurrency limit: {}", e)
            }))
            .into_response();
        }
    };
    let session_id = anthropic_session_id(&req);
    if crate::usage::cap_exceeded(&session_id) {
        return Json(serde_json::json!({
            "error": format!("Usage cap reached for session '{}'", session_id)
        }))
        .into_response();
    }

    let thinking_requested = match req.thinking {
        Some(ThinkingParam::Bool(enabled)) => enabled,
        Some(ThinkingParam::Level(level)) => {
            let _budget = THINKING_LEVELS
                .iter()
                .find(|(k, _)| *k == level)
                .map(|(_, v)| *v);
            true
        }
        Some(ThinkingParam::Object {
            type_,
            budget_tokens,
        }) => {
            let _budget = budget_tokens;
            type_ == "enabled"
        }
        None => false,
    };
    let raw_tools = req.tools.clone().unwrap_or_default();
    let tools = normalize_tools_for_prompt(&raw_tools);
    let tools_enabled = !tools.is_empty();
    let tool_choice = tool_choice_to_prompt_value(req.tool_choice.as_ref());
    tracing::debug!(
        "anthropic request summary: model={}, stream={}, tools_enabled={}, raw_tools={}, tool_choice_present={}, system_present={}, messages={}",
        req.model,
        req.stream,
        tools_enabled,
        raw_tools.len(),
        tool_choice.is_some(),
        req.system.is_some(),
        summarize_anthropic_messages(&req.messages)
    );

    // Convert Anthropic messages to OpenAI format
    let mut openai_messages = Vec::new();

    if tools_enabled {
        openai_messages.push(serde_json::json!({
            "role": "system",
            "content": tools_prompt(&tools, tool_choice.as_ref()),
            "metadata": {
                "leech_proxy_tool_prompt": true
            }
        }));
    } else if let Some(system) = req.system.as_ref() {
        if looks_like_tool_prompt(system) {
            tracing::debug!(
                "Preserving Anthropic tool-like system field as trusted proxy prompt: {}",
                system.to_string()
            );
            openai_messages.push(serde_json::json!({
                "role": "system",
                "content": system,
                "metadata": {
                    "leech_proxy_tool_prompt": true
                }
            }));
        } else {
            tracing::debug!(
                "Dropping Anthropic system field before upstream frame: {}",
                system.to_string()
            );
        }
    }

    for msg in req.messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = convert_anthropic_content(msg.get("content"));
        openai_messages.push(serde_json::json!({
            "role": role,
            "content": content,
        }));
    }
    let tool_mode_expected = tools_enabled || has_trusted_tool_prompt(&openai_messages);
    tracing::debug!(
        "anthropic tool mode summary: explicit_tools={}, trusted_prompt_present={}, tool_mode_expected={}",
        tools_enabled,
        has_trusted_tool_prompt(&openai_messages),
        tool_mode_expected
    );
    tracing::debug!(
        "anthropic converted openai summary: {}",
        summarize_anthropic_messages(&openai_messages)
    );
    let input_tokens = crate::usage::estimate_messages_tokens(&openai_messages);

    let model = resolve_model(&req.model);

    let account = if requires_use_ai_account(&model) {
        match pool.acquire().await {
            Ok(acc) => Some(acc),
            Err(e) => {
                return Json(serde_json::json!({
                    "error": format!("Failed to acquire account: {}", e)
                }))
                .into_response();
            }
        }
    } else {
        None
    };

    let proxy_url = proxy_url_for_model(&model, &pool).await;

    // ---- STREAMING ----
    if req.stream {
        let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
        let model_clone = model.clone();

        if tool_mode_expected {
            let sse_stream = async_stream::stream! {
                let start_event = serde_json::json!({
                    "type": "message_start",
                    "message": {
                        "id": msg_id,
                        "type": "message",
                        "role": "assistant",
                        "content": [],
                        "model": model_clone,
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {
                            "input_tokens": 0,
                            "output_tokens": 0,
                        }
                    }
                });
                yield Ok::<_, Infallible>(axum::response::sse::Event::default().data(start_event.to_string()));
                let ping_event = serde_json::json!({
                    "type": "ping",
                });
                yield Ok(axum::response::sse::Event::default().data(ping_event.to_string()));

                let text_block_start = serde_json::json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "text",
                        "text": "",
                    }
                });
                let mut text_block_open = false;

                let mut stream = stream_completion(CompletionRequest {
                    model: model.clone(),
                    messages: openai_messages.clone(),
                    proxy_url: proxy_url.clone(),
                    account: account.clone(),
                }).await;
                let mut tool_buffer = ToolModeStreamBuffer::default();
                let mut stream_error = None;
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(text) => {
                            for text_part in tool_buffer.push(&text) {
                                if !text_part.is_empty() {
                                    if !text_block_open {
                                        yield Ok(axum::response::sse::Event::default().data(text_block_start.to_string()));
                                        text_block_open = true;
                                    }
                                    let delta = serde_json::json!({
                                        "type": "content_block_delta",
                                        "index": 0,
                                        "delta": {
                                            "type": "text_delta",
                                            "text": text_part,
                                        }
                                    });
                                    yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                                }
                            }
                        }
                        Err(e) => {
                            stream_error = Some(e.to_string());
                            break;
                        }
                    }
                }

                let (reply, held_text) = tool_buffer.finish();

                if let Some(error) = stream_error.as_ref() {
                    if !text_block_open {
                        yield Ok(axum::response::sse::Event::default().data(text_block_start.to_string()));
                        text_block_open = true;
                    }
                    let delta = serde_json::json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {
                            "type": "text_delta",
                            "text": format!("[ERROR] {}", error),
                        }
                    });
                    yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                } else {
                    let parsed_calls = parse_tool_uses(&reply);
                    if parsed_calls.is_empty() {
                        for text_part in held_text {
                            if !text_part.is_empty() {
                                if !text_block_open {
                                    yield Ok(axum::response::sse::Event::default().data(text_block_start.to_string()));
                                    text_block_open = true;
                                }
                                let delta = serde_json::json!({
                                    "type": "content_block_delta",
                                    "index": 0,
                                    "delta": {
                                        "type": "text_delta",
                                        "text": text_part,
                                    }
                                });
                                yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                            }
                        }
                    }
                    let _ = crate::usage::record_tokens(
                        &session_id,
                        &model,
                        input_tokens,
                        crate::usage::estimate_tokens(&reply),
                    );
                }

                if text_block_open {
                    let text_block_stop = serde_json::json!({
                        "type": "content_block_stop",
                        "index": 0,
                    });
                    yield Ok(axum::response::sse::Event::default().data(text_block_stop.to_string()));
                }

                if stream_error.is_none() {
                    let parsed_calls = parse_tool_uses(&reply);
                    if !parsed_calls.is_empty() {
                        for (idx, (name, input)) in parsed_calls.iter().enumerate() {
                            let block_index = idx + 1;
                            let tool_id = format!("toolu_{}", uuid::Uuid::new_v4().simple());
                            let block_start = serde_json::json!({
                                "type": "content_block_start",
                                "index": block_index,
                                "content_block": {
                                    "type": "tool_use",
                                    "id": tool_id,
                                    "name": name,
                                    "input": {},
                                }
                            });
                            yield Ok(axum::response::sse::Event::default().data(block_start.to_string()));
                            let input_json = input.to_string();
                            for partial_json in input_json.as_bytes().chunks(32) {
                                let delta = serde_json::json!({
                                    "type": "content_block_delta",
                                    "index": block_index,
                                    "delta": {
                                        "type": "input_json_delta",
                                        "partial_json": String::from_utf8_lossy(partial_json),
                                    }
                                });
                                yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                            }
                            let block_stop = serde_json::json!({
                                "type": "content_block_stop",
                                "index": block_index,
                            });
                            yield Ok(axum::response::sse::Event::default().data(block_stop.to_string()));
                        }
                        let message_delta = serde_json::json!({
                            "type": "message_delta",
                            "delta": {
                                "stop_reason": "tool_use",
                                "stop_sequence": null,
                            },
                            "usage": {
                                "output_tokens": 0,
                            }
                        });
                        yield Ok(axum::response::sse::Event::default().data(message_delta.to_string()));
                    }
                }

                let message_stop = serde_json::json!({
                    "type": "message_stop",
                });
                yield Ok(axum::response::sse::Event::default().data(message_stop.to_string()));
                yield Ok(axum::response::sse::Event::default().data("[DONE]"));
            };

            return Sse::new(sse_stream).into_response();
        }

        let sse_stream = async_stream::stream! {
            // 1. message_start
            let start_event = serde_json::json!({
                "type": "message_start",
                "message": {
                    "id": msg_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [],
                    "model": model_clone,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": 0,
                        "output_tokens": 0,
                    }
                }
            });
            yield Ok::<_, Infallible>(axum::response::sse::Event::default().data(start_event.to_string()));

            // 2. content_block_start
            let block_start = serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "text",
                    "text": "",
                }
            });
            yield Ok(axum::response::sse::Event::default().data(block_start.to_string()));

            // 3. Stream text deltas, with thinking-aware splitting
            let mut stream = stream_completion(CompletionRequest {
                model: model.clone(),
                messages: openai_messages.clone(),
                proxy_url: proxy_url.clone(),
                account: account.clone(),
            }).await;

            // We'll use a state machine to split thinking and response
            let mut buffer = String::new();
            let mut mode = "unknown"; // "unknown", "thinking", "response"

            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(text) => {
                        buffer.push_str(&text);

                        // Process buffer to extract thinking/response tags
                        while !buffer.is_empty() {
                            if mode == "unknown" {
                                if let Some(idx) = buffer.find("<thinking>") {
                                    // Emit anything before the tag as response (should be empty)
                                    let before = &buffer[..idx];
                                    if !before.is_empty() {
                                        let delta = serde_json::json!({
                                            "type": "content_block_delta",
                                            "index": 0,
                                            "delta": {
                                                "type": "text_delta",
                                                "text": before,
                                            }
                                        });
                                        yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                                    }
                                    buffer = buffer[idx + 10..].to_string(); // skip "<thinking>"
                                    mode = "thinking";
                                } else if let Some(idx) = buffer.find("<response>") {
                                    let before = &buffer[..idx];
                                    if !before.is_empty() {
                                        let delta = serde_json::json!({
                                            "type": "content_block_delta",
                                            "index": 0,
                                            "delta": {
                                                "type": "text_delta",
                                                "text": before,
                                            }
                                        });
                                        yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                                    }
                                    buffer = buffer[idx + 10..].to_string(); // skip "<response>"
                                    mode = "response";
                                } else {
                                    // No tags found; safe to emit everything except a small tail
                                    let guard = 20;
                                    if buffer.len() > guard {
                                        let safe = &buffer[..buffer.len() - guard];
                                        let delta = serde_json::json!({
                                            "type": "content_block_delta",
                                            "index": 0,
                                            "delta": {
                                                "type": "text_delta",
                                                "text": safe,
                                            }
                                        });
                                        yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                                        buffer = buffer[buffer.len() - guard..].to_string();
                                    }
                                    break; // wait for more data
                                }
                            } else if mode == "thinking" {
                                if let Some(idx) = buffer.find("</thinking>") {
                                    let thinking_content = &buffer[..idx];
                                    if !thinking_content.is_empty() {
                                        // Emit thinking_delta
                                        let delta = serde_json::json!({
                                            "type": "thinking_delta",
                                            "index": 0,
                                            "delta": {
                                                "type": "thinking_delta",
                                                "thinking": thinking_content,
                                            }
                                        });
                                        yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                                    }
                                    buffer = buffer[idx + 11..].to_string(); // skip "</thinking>"
                                    mode = "unknown";
                                    // Continue to check for response tag
                                } else {
                                    // Keep a guard, but we can't emit thinking safely without knowing if it will continue
                                    // We'll accumulate and emit in chunks, but for simplicity we'll just hold until closing tag.
                                    // Better: emit thinking_delta events incrementally.
                                    // But to keep it simple, we'll wait for the full thinking.
                                    // However, if buffer grows large, we can flush partial thinking.
                                    // For safety, if buffer len > 1024, we can emit.
                                    if buffer.len() > 1024 {
                                        let safe = &buffer[..buffer.len() - 20];
                                        let delta = serde_json::json!({
                                            "type": "thinking_delta",
                                            "index": 0,
                                            "delta": {
                                                "type": "thinking_delta",
                                                "thinking": safe,
                                            }
                                        });
                                        yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                                        buffer = buffer[buffer.len() - 20..].to_string();
                                    }
                                    break;
                                }
                            } else if mode == "response" {
                                if let Some(idx) = buffer.find("</response>") {
                                    let response_content = &buffer[..idx];
                                    if !response_content.is_empty() {
                                        let delta = serde_json::json!({
                                            "type": "content_block_delta",
                                            "index": 0,
                                            "delta": {
                                                "type": "text_delta",
                                                "text": response_content,
                                            }
                                        });
                                        yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                                    }
                                    buffer = buffer[idx + 11..].to_string(); // skip "</response>"
                                    mode = "unknown";
                                } else {
                                    // Emit response text progressively
                                    if buffer.len() > 20 {
                                        let safe = &buffer[..buffer.len() - 20];
                                        let delta = serde_json::json!({
                                            "type": "content_block_delta",
                                            "index": 0,
                                            "delta": {
                                                "type": "text_delta",
                                                "text": safe,
                                            }
                                        });
                                        yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                                        buffer = buffer[buffer.len() - 20..].to_string();
                                    }
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // Send error as text delta (or just stop)
                        let delta = serde_json::json!({
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {
                                "type": "text_delta",
                                "text": format!("[ERROR] {}", e),
                            }
                        });
                        yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                        break;
                    }
                }
            }

            // Flush any remaining buffer
            if !buffer.is_empty() {
                if mode == "thinking" {
                    // Emit thinking_delta
                    let delta = serde_json::json!({
                        "type": "thinking_delta",
                        "index": 0,
                        "delta": {
                            "type": "thinking_delta",
                            "thinking": buffer,
                        }
                    });
                    yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                    // Emit thinking_block_stop
                    let stop = serde_json::json!({
                        "type": "thinking_block_stop",
                        "index": 0,
                    });
                    yield Ok(axum::response::sse::Event::default().data(stop.to_string()));
                } else {
                    let delta = serde_json::json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {
                            "type": "text_delta",
                            "text": buffer,
                        }
                    });
                    yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                }
            }

            // 4. content_block_stop
            let block_stop = serde_json::json!({
                "type": "content_block_stop",
                "index": 0,
            });
            yield Ok(axum::response::sse::Event::default().data(block_stop.to_string()));

            // 5. message_stop
            let message_stop = serde_json::json!({
                "type": "message_stop",
            });
            yield Ok(axum::response::sse::Event::default().data(message_stop.to_string()));

            // 6. [DONE] (optional but useful for clients)
            yield Ok(axum::response::sse::Event::default().data("[DONE]"));
        };

        return Sse::new(sse_stream).into_response();
    }

    // ---- NON-STREAMING ----
    let result = complete_completion(CompletionRequest {
        model: model.clone(),
        messages: openai_messages.clone(),
        proxy_url: proxy_url.clone(),
        account: account.clone(),
    })
    .await;

    match result {
        Ok(reply) => {
            let _ = crate::usage::record_tokens(
                &session_id,
                &model,
                input_tokens,
                crate::usage::estimate_tokens(&reply),
            );
            if tool_mode_expected {
                let parsed_calls = parse_tool_uses(&reply);
                if !parsed_calls.is_empty() {
                    let resp = serde_json::json!({
                        "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                        "type": "message",
                        "role": "assistant",
                        "content": parsed_calls.iter().map(|(name, input)| serde_json::json!({
                            "type": "tool_use",
                            "id": format!("toolu_{}", uuid::Uuid::new_v4().simple()),
                            "name": name,
                            "input": input,
                        })).collect::<Vec<_>>(),
                        "model": model,
                        "stop_reason": "tool_use",
                        "stop_sequence": null,
                        "usage": {
                            "input_tokens": 0,
                            "output_tokens": 0,
                        },
                    });
                    return Json(resp).into_response();
                }
                if looks_like_tool_call(&reply) {
                    tracing::debug!(
                        "Tool-like Anthropic reply leaked past conversion in non-stream path. raw reply: {}",
                        reply
                    );
                    return Json(serde_json::json!({
                        "error": "Tool call was detected but could not be converted safely"
                    }))
                    .into_response();
                }
                if looks_like_tool_refusal(&reply) {
                    tracing::debug!(
                        "Upstream refused Anthropic tool usage, raw reply: {}",
                        reply
                    );
                }
            }

            let (thinking, response) = parse_thinking(&reply);
            let response = truncate_to_token_budget(response, req.max_tokens);

            let mut resp = serde_json::json!({
                "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                "type": "message",
                "role": "assistant",
                "content": [{
                    "type": "text",
                    "text": response,
                }],
                "model": model,
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {
                    "input_tokens": 0,
                    "output_tokens": response.len() / 4,
                },
            });

            if thinking_requested {
                resp["thinking"] = thinking
                    .map(serde_json::Value::String)
                    .unwrap_or(serde_json::Value::Null);
            }

            Json(resp).into_response()
        }
        Err(e) => Json(serde_json::json!({
            "error": format!("Completion failed: {}", e)
        }))
        .into_response(),
    }
}

/// Parse `<thinking>...</thinking>` and `<response>...</response>` from reply.
fn parse_thinking(reply: &str) -> (Option<String>, String) {
    let thinking_re = regex::Regex::new(r"(?s)<thinking>(.*?)</thinking>").unwrap();
    let response_re = regex::Regex::new(r"(?s)<response>(.*?)</response>").unwrap();
    let thinking = thinking_re
        .captures(reply)
        .map(|cap| cap[1].trim().to_string());
    let response = response_re
        .captures(reply)
        .map(|cap| cap[1].trim().to_string())
        .unwrap_or_else(|| reply.trim().to_string());
    (thinking, response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_content_preserves_prior_tool_use_and_keeps_tool_result() {
        let content = serde_json::json!([
            {
                "type": "tool_use",
                "id": "toolu_1",
                "name": "Bash",
                "input": {"command": "mkdir games"}
            },
            {
                "type": "tool_result",
                "tool_use_id": "toolu_1",
                "content": "Created folders"
            }
        ]);

        let converted = convert_anthropic_content(Some(&content));
        let parts = converted.as_array().unwrap();

        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["type"].as_str().unwrap(), "text");
        assert!(parts[0]["text"].as_str().unwrap().contains("<tool_use>"));
        assert!(parts[0]["text"]
            .as_str()
            .unwrap()
            .contains("\"name\":\"Bash\""));
        assert_eq!(parts[1]["type"].as_str().unwrap(), "text");
        assert!(parts[1]["text"]
            .as_str()
            .unwrap()
            .contains("Tool result for toolu_1 has completed"));
    }

    #[test]
    fn anthropic_file_block_converts_to_internal_file_payload() {
        let content = serde_json::json!([
            {
                "type": "file",
                "name": "notes.txt",
                "source": {
                    "type": "base64",
                    "media_type": "text/plain",
                    "data": "aGVsbG8="
                }
            }
        ]);

        let converted = convert_anthropic_content(Some(&content));
        let parts = converted.as_array().unwrap();

        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"].as_str().unwrap(), "file");
        assert_eq!(parts[0]["file"]["filename"].as_str().unwrap(), "notes.txt");
        assert!(parts[0]["file"]["data"]
            .as_str()
            .unwrap()
            .starts_with("data:text/plain;base64,aGVsbG8="));
    }

    #[test]
    fn anthropic_session_id_prefers_metadata_session_id() {
        let req = AnthropicRequest {
            model: "claude-sonnet-4-6".to_string(),
            messages: vec![],
            metadata: Some(serde_json::json!({
                "session_id": "session-123",
                "user_id": "fallback-user"
            })),
            stream: false,
            system: None,
            max_tokens: None,
            thinking: None,
            tools: None,
            tool_choice: None,
        };

        assert_eq!(anthropic_session_id(&req), "session-123");
    }
}
