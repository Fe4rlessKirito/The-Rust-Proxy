//! Anthropic Messages API compatibility.

use axum::{extract::State, response::IntoResponse, Json};
use serde::Deserialize;

use crate::account_pool::AccountPool;
use crate::direct::complete_completion;
use crate::models::resolve_model;
use crate::pool::acquire_direct_permit;

// Reuse the thinking levels from chat.rs (or define separately)
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
    pub stream: bool,
    #[serde(default)]
    pub system: Option<String>,
    #[serde(default)]
    pub max_tokens: Option<usize>,
    #[serde(default)]
    pub thinking: Option<bool>,
}

pub fn routes() -> axum::Router<AccountPool> {
    axum::Router::new().route("/messages", axum::routing::post(handler))
}

async fn handler(
    State(pool): State<AccountPool>,
    Json(req): Json<AnthropicRequest>,
) -> impl IntoResponse {
    let _permit = match acquire_direct_permit().await {
        Ok(p) => p,
        Err(e) => {
            return Json(serde_json::json!({
                "error": format!("Concurrency limit: {}", e)
            }));
        }
    };

    let thinking_enabled = req.thinking.unwrap_or(false);

    // Convert Anthropic messages to OpenAI format
    let mut openai_messages = Vec::new();

    // If a system message is provided, use it; otherwise we'll inject thinking into an empty system.
    if let Some(system) = req.system {
        openai_messages.push(serde_json::json!({
            "role": "system",
            "content": system,
        }));
    }

    for msg in req.messages {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = match msg.get("content") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(arr)) => {
                let texts: Vec<String> = arr
                    .iter()
                    .filter_map(|item| {
                        if item.get("type").and_then(|v| v.as_str()) == Some("text") {
                            item.get("text").and_then(|v| v.as_str()).map(String::from)
                        } else {
                            None
                        }
                    })
                    .collect();
                texts.join(" ")
            }
            _ => String::new(),
        };
        openai_messages.push(serde_json::json!({
            "role": role,
            "content": content,
        }));
    }

    // --- INJECT THINKING PROMPT (if enabled) ---
    // Reuse the same logic as in chat.rs.
    if thinking_enabled {
        // Build thinking prompt
        let level = "medium"; // default; could be extended to accept 'budget_tokens' from Anthropic request
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

        // If there's already a system message, append to it; otherwise insert one.
        if let Some(first) = openai_messages.first_mut() {
            if first.get("role") == Some(&"system".into()) {
                if let Some(content) = first.get_mut("content") {
                    if let Some(s) = content.as_str() {
                        *content = serde_json::Value::String(format!("{}\n\n{}", s, thinking_prompt));
                    }
                }
            } else {
                openai_messages.insert(0, serde_json::json!({
                    "role": "system",
                    "content": thinking_prompt,
                }));
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
            }));
        }
    };

    let proxy_url = pool.next_proxy().await;

    let result = complete_completion(&model, &openai_messages, proxy_url.as_deref(), account).await;

    match result {
        Ok(reply) => {
            // Parse thinking from reply (if enabled)
            let (thinking, response) = if thinking_enabled {
                parse_thinking(&reply)
            } else {
                (None, reply)
            };

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

            // If thinking is enabled and we have parsed thinking, include it.
            if thinking_enabled {
                if let Some(t) = thinking {
                    resp["thinking"] = serde_json::Value::String(t);
                } else {
                    // If no thinking tags were found, we might still include an empty field
                    // or we can omit it. We'll include it as null for consistency.
                    resp["thinking"] = serde_json::Value::Null;
                }
            }

            Json(resp)
        }
        Err(e) => Json(serde_json::json!({
            "error": format!("Completion failed: {}", e)
        })),
    }
}

/// Parse `<thinking>...</thinking>` and `<response>...</response>` from reply.
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
