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

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub thinking: Option<ThinkingParam>,
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

async fn handler(
    State(pool): State<AccountPool>,
    Json(req): Json<ChatRequest>,
) -> Response {
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
            let cfg = crate::config::Config::default();
            let budget = cfg.thinking.levels.get(&level).copied().unwrap_or(1024);
            (true, budget)
        }
        Some(ThinkingParam::Object { type_, budget_tokens }) => {
            (type_ == "enabled", budget_tokens)
        }
        None => (false, 1024),
    };

    let mut messages = req.messages;
    if thinking_enabled {
        let thinking_prompt = "Before you answer, reason step by step. Format your response exactly as:\n\n<thinking>\nYour reasoning here.\n</thinking>\n\n<response>\nYour final answer here.\n</response>";
        let system_msg = serde_json::json!({
            "role": "system",
            "content": thinking_prompt,
        });

        if let Some(first) = messages.first_mut() {
            if first.get("role") == Some(&"system".into()) {
                if let Some(content) = first.get_mut("content") {
                    if let Some(s) = content.as_str() {
                        *content = serde_json::Value::String(format!("{}\n\n{}", s, thinking_prompt));
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
        let model = model.clone();
        let messages = messages.clone();
        let sse_stream = async_stream::stream! {
            let _permit = permit;
            let mut stream = stream_completion(
                &model,
                &messages,
                proxy_url.as_deref(),
                account,
            )
            .await;
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(text) => {
                        yield Ok::<_, Infallible>(Event::default().data(text));
                    }
                    Err(e) => {
                        yield Ok(Event::default().data(format!("[ERROR] {}", e)));
                        break;
                    }
                }
            }
            yield Ok::<_, Infallible>(Event::default().data("[DONE]"));
        };
        Sse::new(sse_stream).into_response()
    } else {
        match complete_completion(&model, &messages, proxy_url.as_deref(), account).await {
            Ok(reply) => {
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

fn parse_thinking(reply: &str) -> (Option<String>, String) {
    let thinking_re = regex::Regex::new(r"(?s)<thinking>(.*?)</thinking>").unwrap();
    let response_re = regex::Regex::new(r"(?s)<response>(.*?)</response>").unwrap();
    let thinking = thinking_re.captures(reply).map(|cap| cap[1].trim().to_string());
    let response = response_re
        .captures(reply)
        .map(|cap| cap[1].trim().to_string())
        .unwrap_or_else(|| reply.trim().to_string());
    (thinking, response)
}
