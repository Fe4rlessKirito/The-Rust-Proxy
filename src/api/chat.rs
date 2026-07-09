//! OpenAI-compatible /v1/chat/completions endpoint.

use axum::{
    extract::State,
    http::StatusCode,
    response::{sse::Event, IntoResponse, Response, Sse},
    Json,
};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

use super::format::{parse_thinking, split_stream_text};
use super::tools::{
    has_trusted_tool_prompt, is_tool_call_incomplete, looks_like_tool_call, looks_like_tool_prompt,
    looks_like_tool_refusal, mark_trusted_tool_prompt, normalize_tool_messages,
    normalize_tools_for_prompt, parse_tool_uses, should_suppress_tool_text,
    tool_choice_to_prompt_value, tools_prompt, ToolModeStreamBuffer,
};

use crate::account_pool::AccountPool;
use crate::models::resolve_model;
use crate::pool::acquire_direct_permit;
use crate::providers::{
    complete_completion, proxy_url_for_model, requires_use_ai_account, stream_completion,
    CompletionRequest,
};

#[derive(Debug, Deserialize, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub thinking: Option<ThinkingParam>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
}

#[derive(Debug, Deserialize, Clone)]
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
    axum::Router::new().route("/chat/completions", axum::routing::post(chat_handler))
}

fn chat_session_id(req: &ChatRequest) -> String {
    req.user
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string()
}

async fn chat_handler(State(pool): State<AccountPool>, Json(req): Json<ChatRequest>) -> Response {
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
    let session_id = chat_session_id(&req);
    if crate::usage::cap_exceeded(&session_id) {
        return Json(serde_json::json!({
            "error": format!("Usage cap reached for session '{}'", session_id)
        }))
        .into_response();
    }

    let thinking_requested = match req.thinking {
        Some(ThinkingParam::Bool(b)) => b,
        Some(ThinkingParam::Level(level)) => {
            let cfg = crate::config::Config::load().unwrap_or_default();
            let _budget = cfg.thinking.levels.get(&level).copied().unwrap_or(1024);
            true
        }
        Some(ThinkingParam::Object {
            type_,
            budget_tokens,
        }) => {
            let _budget = budget_tokens.unwrap_or(1024);
            type_ == "enabled"
        }
        None => false,
    };

    let raw_tools = req.tools.clone().unwrap_or_default();
    let tools = normalize_tools_for_prompt(&raw_tools);
    let tools_enabled = !tools.is_empty();
    debug!(
        "Incoming request (responses API): {} tools present, tools_enabled={}, message_count={}",
        raw_tools.len(),
        tools_enabled,
        req.messages.len()
    );
    let tool_choice = tool_choice_to_prompt_value(req.tool_choice.as_ref());
    let mut messages = req.messages;
    normalize_tool_messages(&mut messages);

    if tools_enabled {
        messages.insert(
            0,
            serde_json::json!({
                "role": "system",
                "content": tools_prompt(&tools, tool_choice.as_ref()),
                "metadata": {
                    "leech_proxy_tool_prompt": true
                }
            }),
        );
    } else {
        messages.retain_mut(|message| {
            if message.get("role").and_then(|v| v.as_str()) == Some("system") {
                if message
                    .get("content")
                    .map(looks_like_tool_prompt)
                    .unwrap_or(false)
                {
                    tracing::debug!(
                        "Preserving inbound tool-like system prompt as trusted proxy prompt"
                    );
                    mark_trusted_tool_prompt(message);
                    return true;
                }

                tracing::debug!(
                    "Dropping chat system message before direct completion: {}",
                    message
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("<non-string>")
                        .chars()
                        .take(160)
                        .collect::<String>()
                );
                false
            } else {
                true
            }
        });
    }

    let input_tokens = crate::usage::estimate_messages_tokens(&messages);

    let tool_mode_expected = tools_enabled || has_trusted_tool_prompt(&messages);
    debug!(
        "chat tool mode summary: explicit_tools={}, trusted_prompt_present={}, tool_mode_expected={}",
        tools_enabled,
        has_trusted_tool_prompt(&messages),
        tool_mode_expected
    );

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

    if req.stream {
        // Generate a chat completion ID and timestamp (shared for all chunks)
        let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
        let created = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let model_clone = model.clone();

        // Open the upstream use.ai stream and peek its first item *before*
        // committing the SSE 200. A fatal per-IP rate limit (no content
        // emitted, not retried because use.ai binds the session to one Tor
        // exit) must surface as a real HTTP 429 so the client retries with a
        // fresh account/exit — otherwise we'd emit an empty
        // `finish_reason: "stop"` completion that clients report as "model
        // returned no content". Mid-stream rate limits (after content) are
        // still ended cleanly inside the generator below.
        let upstream = stream_completion(CompletionRequest {
            model: model.clone(),
            messages: messages.clone(),
            proxy_url: proxy_url.clone(),
            account: account.clone(),
        })
        .await;
        let stream = match crate::direct::peek_stream_start(upstream).await {
            Ok(s) => s,
            Err(_e) => {
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(serde_json::json!({
                        "error": {
                            "message": "upstream use.ai rate limit; please retry",
                            "type": "rate_limit_error",
                        }
                    })),
                )
                    .into_response();
            }
        };

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

            let mut stream = stream;

            let mut buffered_reply = String::new();
            let mut tool_buffer = ToolModeStreamBuffer::default();
            let mut stream_failed = false;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(text) => {
                        if tool_mode_expected {
                            for text_part in tool_buffer.push(&text) {
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
                            continue;
                        }
                        buffered_reply.push_str(&text);
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
                        stream_failed = true;
                        // A use.ai rate-limit error must not leak into the
                        // model's content stream. Either it was already
                        // retried transparently upstream, or (mid-stream) we
                        // just end the reply cleanly. Other errors stay
                        // visible as an [ERROR] delta for debuggability.
                        if !crate::direct::is_stream_rate_limit_error(&e) {
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
                        }
                        break;
                    }
                }
            }

            if tool_mode_expected {
                let (finished_reply, held_text) = tool_buffer.finish();
                buffered_reply = finished_reply;
                if !stream_failed {
                    let parsed_calls = parse_tool_uses(&buffered_reply);
                    if !parsed_calls.is_empty() {
                        let _ = crate::usage::record_tokens(
                            &session_id,
                            &model,
                            input_tokens,
                            crate::usage::estimate_tokens(&buffered_reply),
                        );
                        let tool_call_ids = parsed_calls
                            .iter()
                            .map(|_| format!("call_{}", uuid::Uuid::new_v4().simple()))
                            .collect::<Vec<_>>();
                        let name_chunk = serde_json::json!({
                            "id": id,
                            "object": "chat.completion.chunk",
                            "created": created,
                            "model": model_clone,
                            "choices": [{
                                "index": 0,
                                "delta": {
                                    "tool_calls": parsed_calls.iter().enumerate().map(|(idx, (name, _))| serde_json::json!({
                                        "index": idx,
                                        "id": tool_call_ids[idx],
                                        "type": "function",
                                        "function": {
                                            "name": name,
                                            "arguments": "",
                                        }
                                    })).collect::<Vec<_>>()
                                },
                                "finish_reason": null,
                            }]
                        });
                        yield Ok::<_, Infallible>(Event::default().data(name_chunk.to_string()));

                        for (idx, (_, input)) in parsed_calls.iter().enumerate() {
                            let arguments = input.to_string();
                            for arg_part in split_stream_text(&arguments) {
                                let arg_chunk = serde_json::json!({
                                    "id": id,
                                    "object": "chat.completion.chunk",
                                    "created": created,
                                    "model": model_clone,
                                    "choices": [{
                                        "index": 0,
                                        "delta": {
                                            "tool_calls": [{
                                                "index": idx,
                                                "function": {
                                                    "arguments": arg_part,
                                                }
                                            }]
                                        },
                                        "finish_reason": null,
                                    }]
                                });
                                yield Ok::<_, Infallible>(Event::default().data(arg_chunk.to_string()));
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
                                "finish_reason": "tool_calls",
                            }]
                        });
                        yield Ok::<_, Infallible>(Event::default().data(final_chunk.to_string()));
                        yield Ok::<_, Infallible>(Event::default().data("[DONE]"));
                        return;
                    }

                    if is_tool_call_incomplete(&buffered_reply) {
                        debug!("Incomplete tool call from upstream, raw reply: {}", buffered_reply);
                    }

                    if looks_like_tool_call(&buffered_reply) {
                        debug!("Unconvertible tool call from upstream, raw reply: {}", buffered_reply);
                    }

                    if looks_like_tool_refusal(&buffered_reply) {
                        debug!("Upstream refused tool usage, raw reply: {}", buffered_reply);
                    }
                }

                if !stream_failed && should_suppress_tool_text(&buffered_reply) {
                    debug!("Suppressing unconverted tool-like stream reply: {}", buffered_reply);
                } else {
                    for text_part in held_text {
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
            }

            let _ = crate::usage::record_tokens(
                &session_id,
                &model,
                input_tokens,
                crate::usage::estimate_tokens(&buffered_reply),
            );

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
        match complete_completion(CompletionRequest {
            model: model.clone(),
            messages: messages.clone(),
            proxy_url: proxy_url.clone(),
            account: account.clone(),
        })
        .await
        {
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
                                    "tool_calls": parsed_calls.iter().map(|(name, input)| serde_json::json!({
                                        "id": format!("call_{}", uuid::Uuid::new_v4().simple()),
                                        "type": "function",
                                        "function": {
                                            "name": name,
                                            "arguments": input.to_string(),
                                        }
                                    })).collect::<Vec<_>>()
                                },
                                "finish_reason": "tool_calls",
                            }],
                        });
                        return Json(json_reply).into_response();
                    }
                    if looks_like_tool_call(&reply) {
                        debug!(
                            "Tool-like reply leaked past conversion in non-stream path. raw reply: {}",
                            reply
                        );
                        return Json(serde_json::json!({
                            "error": "Tool call was detected but could not be converted safely"
                        }))
                        .into_response();
                    }
                    if looks_like_tool_refusal(&reply) {
                        debug!("Upstream refused tool usage, raw reply: {}", reply);
                        // Fall through to the normal completion below instead of
                        // returning a synthetic error -- see streaming handler for
                        // why (poisons later turns if the client stores/replays it).
                    }
                }

                let (thinking, response) = parse_thinking(&reply);
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
                if thinking_requested {
                    if let Some(t) = thinking {
                        json_reply["thinking"] = serde_json::Value::String(t);
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_session_id_uses_user_when_present() {
        let req = ChatRequest {
            model: "gpt-5-4".to_string(),
            messages: vec![],
            user: Some("session-123".to_string()),
            stream: false,
            thinking: None,
            tools: None,
            tool_choice: None,
        };

        assert_eq!(chat_session_id(&req), "session-123");
    }
}
