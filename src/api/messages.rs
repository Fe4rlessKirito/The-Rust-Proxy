//! Anthropic Messages API compatibility.

use axum::{
    extract::State,
    response::{IntoResponse, Response, Sse},
    Json,
};
use futures::StreamExt;
use serde::Deserialize;
use std::convert::Infallible;

use crate::account_pool::AccountPool;
use crate::direct::{complete_completion, stream_completion};
use crate::models::resolve_model;
use crate::pool::acquire_direct_permit;

// Thinking level to budget (same as chat.rs)
const THINKING_LEVELS: &[(&str, usize)] = &[
    ("low", 1024),
    ("medium", 5000),
    ("high", 16000),
    ("max", 32000),
];

const TOOL_PROMPT: &str = r#"You may be given tools. If a tool is needed, do not answer with prose. Instead output exactly one tool call in this format:

<tool_use>
{"name":"tool_name","input":{"key":"value"}}
</tool_use>

After a tool result is provided, answer the user normally or request another tool with the same format."#;

#[derive(Debug, Deserialize)]
pub struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<usize>,
    #[serde(default)]
    pub thinking: Option<bool>,
    #[serde(default)]
    pub tools: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
}

pub fn routes() -> axum::Router<AccountPool> {
    axum::Router::new().route("/messages", axum::routing::post(handler))
}

fn tools_prompt(tools: &[serde_json::Value], tool_choice: Option<&serde_json::Value>) -> String {
    let mut prompt = String::from(TOOL_PROMPT);
    prompt.push_str("\n\nAvailable tools:\n");
    prompt.push_str(
        &serde_json::to_string_pretty(tools).unwrap_or_else(|_| "[]".to_string()),
    );
    if let Some(choice) = tool_choice {
        prompt.push_str("\n\nTool choice:\n");
        prompt.push_str(&choice.to_string());
    }
    prompt
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
                            "text": format!("Tool result for {}:\n{}", tool_use_id, result_content),
                        }))
                    }
                    Some("tool_use") => {
                        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("tool");
                        let input = item.get("input").cloned().unwrap_or(serde_json::Value::Null);
                        Some(serde_json::json!({
                            "type": "text",
                            "text": format!("Assistant requested tool {} with input: {}", name, input),
                        }))
                    }
                    Some("file") => Some(item.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            serde_json::Value::Array(parts)
        }
        Some(other) => other.clone(),
        None => serde_json::Value::String(String::new()),
    }
}

fn parse_tool_use(reply: &str) -> Option<(String, serde_json::Value)> {
    let value = extract_tagged_json(reply, "tool_use")?;
    let name = value.get("name")?.as_str()?.to_string();
    let input = value
        .get("input")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Some((name, input))
}

fn extract_tagged_json(reply: &str, tag: &str) -> Option<serde_json::Value> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = reply.find(&open)? + open.len();
    let end = reply[start..].find(&close)? + start;
    let body = reply[start..end].trim();
    serde_json::from_str::<serde_json::Value>(body).ok()
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

    let thinking_enabled = req.thinking.unwrap_or(false);
    let tools = req.tools.clone().unwrap_or_default();
    let tools_enabled = !tools.is_empty();

    // Convert Anthropic messages to OpenAI format
    let mut openai_messages = Vec::new();

    let mut system_parts = Vec::new();
    if let Some(system) = req.system {
        system_parts.push(system);
    }
    if tools_enabled {
        system_parts.push(tools_prompt(&tools, req.tool_choice.as_ref()));
    }
    if !system_parts.is_empty() {
        openai_messages.push(serde_json::json!({
            "role": "system",
            "content": system_parts.join("\n\n"),
        }));
    }

    for msg in req.messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = convert_anthropic_content(msg.get("content"));
        openai_messages.push(serde_json::json!({
            "role": role,
            "content": content,
        }));
    }

    // --- INJECT THINKING PROMPT ---
    if thinking_enabled {
        let level = "medium";
        let _budget = THINKING_LEVELS
            .iter()
            .find(|(k, _)| *k == level)
            .map(|(_, v)| *v)
            .unwrap_or(1024);
        let depth = match level {
            "low" => "briefly",
            "medium" => "step by step",
            "high" => "thoroughly, exploring multiple angles",
            "max" => "exhaustively, considering all possible angles and edge cases",
            _ => "step by step",
        };
        let thinking_prompt = format!(
            "Before you answer, reason {}. Format your response exactly as:\n\n\
            <thinking>\nYour reasoning here.\n</thinking>\n\n\
            <response>\nYour final answer here.\n</response>",
            depth
        );

        if let Some(first) = openai_messages.first_mut() {
            if first.get("role") == Some(&"system".into()) {
                if let Some(content) = first.get_mut("content") {
                    if let Some(s) = content.as_str() {
                        *content =
                            serde_json::Value::String(format!("{}\n\n{}", s, thinking_prompt));
                    }
                }
            } else {
                openai_messages.insert(
                    0,
                    serde_json::json!({
                        "role": "system",
                        "content": thinking_prompt,
                    }),
                );
            }
        } else {
            openai_messages.push(serde_json::json!({
                "role": "system",
                "content": thinking_prompt,
            }));
        }
    }

    let model = resolve_model(&req.model);

    let account = match pool.acquire().await {
        Ok(acc) => acc,
        Err(e) => {
            return Json(serde_json::json!({
                "error": format!("Failed to acquire account: {}", e)
            }))
            .into_response();
        }
    };

    let proxy_url = pool.next_proxy().await;

    // ---- STREAMING ----
    if req.stream {
        let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());
        let model_clone = model.clone();

        if tools_enabled {
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

                let mut stream = stream_completion(
                    &model,
                    &openai_messages,
                    proxy_url.as_deref(),
                    account,
                ).await;
                let mut reply = String::new();
                let mut stream_error = None;
                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(text) => reply.push_str(&text),
                        Err(e) => {
                            stream_error = Some(e.to_string());
                            break;
                        }
                    }
                }

                if let Some(error) = stream_error {
                    let block_start = serde_json::json!({
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {
                            "type": "text",
                            "text": "",
                        }
                    });
                    yield Ok(axum::response::sse::Event::default().data(block_start.to_string()));
                    let delta = serde_json::json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {
                            "type": "text_delta",
                            "text": format!("[ERROR] {}", error),
                        }
                    });
                    yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                    let block_stop = serde_json::json!({
                        "type": "content_block_stop",
                        "index": 0,
                    });
                    yield Ok(axum::response::sse::Event::default().data(block_stop.to_string()));
                } else if let Some((name, input)) = parse_tool_use(&reply) {
                    let tool_id = format!("toolu_{}", uuid::Uuid::new_v4().simple());
                    let block_start = serde_json::json!({
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {
                            "type": "tool_use",
                            "id": tool_id,
                            "name": name,
                            "input": {},
                        }
                    });
                    yield Ok(axum::response::sse::Event::default().data(block_start.to_string()));
                    let input_json = input.to_string();
                    let delta = serde_json::json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {
                            "type": "input_json_delta",
                            "partial_json": input_json,
                        }
                    });
                    yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                    let block_stop = serde_json::json!({
                        "type": "content_block_stop",
                        "index": 0,
                    });
                    yield Ok(axum::response::sse::Event::default().data(block_stop.to_string()));
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
                } else {
                    let block_start = serde_json::json!({
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {
                            "type": "text",
                            "text": "",
                        }
                    });
                    yield Ok(axum::response::sse::Event::default().data(block_start.to_string()));
                    if !reply.is_empty() {
                        let delta = serde_json::json!({
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {
                                "type": "text_delta",
                                "text": reply,
                            }
                        });
                        yield Ok(axum::response::sse::Event::default().data(delta.to_string()));
                    }
                    let block_stop = serde_json::json!({
                        "type": "content_block_stop",
                        "index": 0,
                    });
                    yield Ok(axum::response::sse::Event::default().data(block_stop.to_string()));
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
            let mut stream = stream_completion(
                &model,
                &openai_messages,
                proxy_url.as_deref(),
                account,
            ).await;

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
    let result = complete_completion(&model, &openai_messages, proxy_url.as_deref(), account).await;

    match result {
        Ok(reply) => {
            if tools_enabled {
                if let Some((name, input)) = parse_tool_use(&reply) {
                    let resp = serde_json::json!({
                        "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
                        "type": "message",
                        "role": "assistant",
                        "content": [{
                            "type": "tool_use",
                            "id": format!("toolu_{}", uuid::Uuid::new_v4().simple()),
                            "name": name,
                            "input": input,
                        }],
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
            }

            let (thinking, response) = if thinking_enabled {
                parse_thinking(&reply)
            } else {
                (None, reply)
            };
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

            if thinking_enabled {
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
