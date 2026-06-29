//! OpenAI-compatible /v1/chat/completions endpoint.

use axum::{
    extract::State,
    response::{sse::Event, IntoResponse, Response, Sse},
    Json,
};
use futures::StreamExt;
use serde::Deserialize;
use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::account_pool::AccountPool;
use crate::direct::{complete_completion, stream_completion};
use crate::models::resolve_model;
use crate::pool::acquire_direct_permit;

const STREAM_CHUNK_CHARS: usize = 32;
const TOOL_PROMPT: &str = r#"You may be given tools. If a tool is needed, do not answer with prose. Instead output exactly one tool call in this format:

<tool_use>
{"name":"tool_name","input":{"key":"value"}}
</tool_use>

After a tool result is provided, answer the user normally or request another tool with the same format."#;

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    #[serde(default)]
    pub stream: bool,
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
    Object { type_: String, budget_tokens: usize },
}

pub fn routes() -> axum::Router<AccountPool> {
    axum::Router::new().route("/chat/completions", axum::routing::post(handler))
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

fn normalize_tool_messages(messages: &mut [serde_json::Value]) {
    for msg in messages {
        if msg.get("role").and_then(|v| v.as_str()) == Some("tool") {
            let tool_call_id = msg
                .get("tool_call_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let content = msg
                .get("content")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned)
                .or_else(|| msg.get("content").map(|v| v.to_string()))
                .unwrap_or_default();
            *msg = serde_json::json!({
                "role": "user",
                "content": format!("Tool result for {}:\n{}", tool_call_id, content),
            });
        }
    }
}

async fn handler(State(pool): State<AccountPool>, Json(req): Json<ChatRequest>) -> Response {
    let permit = match acquire_direct_permit().await {
        Ok(p) => p,
        Err(e) => {
            return Json(serde_json::json!({
                "error": format!("Concurrency limit error: {}", e)
            }))
            .into_response();
        }
    };

    let model = resolve_model(&req.model);

    let (thinking_enabled, _budget) = match req.thinking {
        Some(ThinkingParam::Bool(b)) => (b, 1024),
        Some(ThinkingParam::Level(level)) => {
            let cfg = crate::config::Config::load().unwrap_or_default();
            let budget = cfg.thinking.levels.get(&level).copied().unwrap_or(1024);
            (true, budget)
        }
        Some(ThinkingParam::Object {
            type_,
            budget_tokens,
        }) => (type_ == "enabled", budget_tokens),
        None => (false, 1024),
    };

    let tools = req.tools.clone().unwrap_or_default();
    let tools_enabled = !tools.is_empty();
    let mut messages = req.messages;
    normalize_tool_messages(&mut messages);

    let mut injected_prompts = Vec::new();
    if tools_enabled {
        injected_prompts.push(tools_prompt(&tools, req.tool_choice.as_ref()));
    }
    if thinking_enabled {
        injected_prompts.push("Before you answer, reason step by step. Format your response exactly as:\n\n<thinking>\nYour reasoning here.\n</thinking>\n\n<response>\nYour final answer here.\n</response>".to_string());
    }

    if !injected_prompts.is_empty() {
        let injected_prompt = injected_prompts.join("\n\n");
        let system_msg = serde_json::json!({
            "role": "system",
            "content": injected_prompt,
        });

        if let Some(first) = messages.first_mut() {
            if first.get("role") == Some(&"system".into()) {
                if let Some(content) = first.get_mut("content") {
                    if let Some(s) = content.as_str() {
                        *content =
                            serde_json::Value::String(format!("{}\n\n{}", s, injected_prompt));
                    }
                }
            } else {
                messages.insert(0, system_msg);
            }
        } else {
            messages.push(system_msg);
        }
    }

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

    if req.stream {
        // Generate a chat completion ID and timestamp (shared for all chunks)
        let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let model_clone = model.clone();

        let sse_stream = async_stream::stream! {
            let _permit = permit;

            let role_chunk = serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model_clone,
                "choices": [{
                    "index": 0,
                    "delta": {
                        "role": "assistant"
                    },
                    "finish_reason": null,
                }]
            });
            yield Ok::<_, Infallible>(Event::default().data(role_chunk.to_string()));

            let mut stream = stream_completion(
                &model,
                &messages,
                proxy_url.as_deref(),
                account,
            )
            .await;

            let mut buffered_reply = String::new();
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(text) => {
                        if tools_enabled {
                            buffered_reply.push_str(&text);
                            continue;
                        }
                        for text_part in split_stream_text(&text) {
                            let mut delta = serde_json::Map::new();
                            delta.insert("content".to_string(), serde_json::Value::String(text_part));

                            let chunk_obj = serde_json::json!({
                                "id": id,
                                "object": "chat.completion.chunk",
                                "created": created,
                                "model": model_clone,
                                "choices": [{
                                    "index": 0,
                                    "delta": delta,
                                    "finish_reason": null,
                                }]
                            });
                            yield Ok::<_, Infallible>(Event::default().data(chunk_obj.to_string()));
                        }
                    }
                    Err(e) => {
                        let error_chunk = serde_json::json!({
                            "id": id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model_clone,
                            "choices": [{
                                "index": 0,
                                "delta": {
                                    "content": format!("[ERROR] {}", e)
                                },
                                "finish_reason": null,
                            }]
                        });
                        yield Ok(Event::default().data(error_chunk.to_string()));
                        break;
                    }
                }
            }

            if tools_enabled {
                if let Some((name, input)) = parse_tool_use(&buffered_reply) {
                    let tool_call_id = format!("call_{}", uuid::Uuid::new_v4().simple());
                    let tool_call_chunk = serde_json::json!({
                        "id": id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": model_clone,
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "tool_calls": [{
                                    "index": 0,
                                    "id": tool_call_id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": input.to_string(),
                                    }
                                }]
                            },
                            "finish_reason": null,
                        }]
                    });
                    yield Ok::<_, Infallible>(Event::default().data(tool_call_chunk.to_string()));
                    let final_chunk = serde_json::json!({
                        "id": id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": model_clone,
                        "choices": [{
                            "index": 0,
                            "delta": {},
                            "finish_reason": "tool_calls",
                        }]
                    });
                    yield Ok::<_, Infallible>(Event::default().data(final_chunk.to_string()));
                    yield Ok::<_, Infallible>(Event::default().data("[DONE]"));
                    return;
                }

                for text_part in split_stream_text(&buffered_reply) {
                    let chunk_obj = serde_json::json!({
                        "id": id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": model_clone,
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "content": text_part,
                            },
                            "finish_reason": null,
                        }]
                    });
                    yield Ok::<_, Infallible>(Event::default().data(chunk_obj.to_string()));
                }
            }

            let final_chunk = serde_json::json!({
                "id": id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model_clone,
                "choices": [{
                    "index": 0,
                    "delta": {},
                    "finish_reason": "stop",
                }]
            });
            yield Ok::<_, Infallible>(Event::default().data(final_chunk.to_string()));

            yield Ok::<_, Infallible>(Event::default().data("[DONE]"));
        };

        Sse::new(sse_stream).into_response()
    } else {
        // Non-streaming (unchanged)
        match complete_completion(&model, &messages, proxy_url.as_deref(), account).await {
            Ok(reply) => {
                if tools_enabled {
                    if let Some((name, input)) = parse_tool_use(&reply) {
                        let json_reply = serde_json::json!({
                            "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
                            "object": "chat.completion",
                            "created": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                            "model": model,
                            "choices": [{
                                "index": 0,
                                "message": {
                                    "role": "assistant",
                                    "content": null,
                                    "tool_calls": [{
                                        "id": format!("call_{}", uuid::Uuid::new_v4().simple()),
                                        "type": "function",
                                        "function": {
                                            "name": name,
                                            "arguments": input.to_string(),
                                        }
                                    }]
                                },
                                "finish_reason": "tool_calls",
                            }],
                        });
                        return Json(json_reply).into_response();
                    }
                }

                let (thinking, response) = if thinking_enabled {
                    parse_thinking(&reply)
                } else {
                    (None, reply)
                };
                let mut json_reply = serde_json::json!({
                    "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
                    "object": "chat.completion",
                    "created": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": response,
                        },
                        "finish_reason": "stop",
                    }],
                });
                if let Some(t) = thinking {
                    json_reply["thinking"] = serde_json::Value::String(t);
                }
                Json(json_reply).into_response()
            }
            Err(e) => Json(serde_json::json!({
                "error": format!("Completion failed: {}", e)
            }))
            .into_response(),
        }
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

fn split_stream_text(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if current.chars().count() >= STREAM_CHUNK_CHARS {
            chunks.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}
