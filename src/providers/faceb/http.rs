use anyhow::Result;
use futures::stream::BoxStream;
use serde_json::{json, Value};
use std::time::Duration;

use crate::providers::CompletionRequest;

use super::account::build_client;
use super::pool;
use super::sse::stream_faceb_sse_response;
use super::{FACEB_API, FACEB_KEY_ATTEMPTS, FACEB_POOL_WAIT_MS};

pub(super) async fn faceb_stream_completion(
    request: CompletionRequest,
) -> Result<BoxStream<'static, Result<String>>> {
    let payload = json!({"model": request.model, "messages": request.messages, "stream": true});
    let response = faceb_post_with_rotation(payload, request.proxy_url.as_deref(), true).await?;
    Ok(Box::pin(stream_faceb_sse_response(response)))
}

pub(super) async fn faceb_post_with_rotation(
    payload: Value,
    proxy_url: Option<&str>,
    stream: bool,
) -> Result<reqwest::Response> {
    for _ in 0..FACEB_KEY_ATTEMPTS {
        let Some(key) = pool::get_key(proxy_url).await else {
            tokio::time::sleep(Duration::from_millis(FACEB_POOL_WAIT_MS)).await;
            continue;
        };
        let client = build_client(proxy_url, Duration::from_secs(120))?;
        let response = client
            .post(format!("{}/chat/completions", FACEB_API))
            .bearer_auth(&key)
            .header("Content-Type", "application/json")
            .header(
                "Accept",
                if stream {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
            .json(&payload)
            .send()
            .await;
        let Ok(response) = response else {
            pool::set_last_error("Faceb request network error").await;
            continue;
        };
        if matches!(
            response.status().as_u16(),
            401 | 402 | 403 | 429 | 500 | 502 | 503
        ) {
            pool::mark_dead();
            pool::set_last_error(format!(
                "Faceb key rejected with HTTP {}",
                response.status()
            ))
            .await;
            continue;
        }
        if stream {
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_lowercase();
            if content_type.contains("text/event-stream") {
                return Ok(response);
            }
            let status = response.status();
            let data = response.json::<Value>().await.unwrap_or_else(|_| json!({}));
            if is_dead_response(&data) {
                pool::mark_dead();
                pool::set_last_error("Faceb key exhausted").await;
                continue;
            }
            anyhow::bail!(
                "Faceb returned non-SSE stream response {}: {}",
                status,
                data
            );
        }
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Faceb returned {}: {}", status, body);
        }
        pool::clear_last_error().await;
        return Ok(response);
    }
    pool::set_last_error("all Faceb keys exhausted").await;
    anyhow::bail!("all Faceb keys exhausted")
}

pub(super) fn is_dead_response(data: &Value) -> bool {
    let err = match data.get("error") {
        Some(Value::String(text)) => text.to_lowercase(),
        Some(Value::Object(obj)) => obj
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_lowercase(),
        _ => String::new(),
    };
    [
        "credit",
        "out of",
        "exhausted",
        "quota",
        "rate limit",
        "rate_limit",
        "unauthorized",
        "invalid api key",
        "invalid key",
        "blocked",
        "denied",
        "exceeded",
        "insufficient",
        "revoked",
    ]
    .iter()
    .any(|needle| err.contains(needle))
}
