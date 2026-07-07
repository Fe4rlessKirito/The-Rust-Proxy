//! FreeModel.dev provider adapter.

use anyhow::{anyhow, Context, Result};
use futures::stream::{self, BoxStream};
use futures::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::openai_compat::{extract_chat_content, parse_sse_delta};
use super::{CompletionProvider, CompletionRequest, ProviderPoolStats};

const FREEMODEL_OPENAI_API: &str = "https://api.freemodel.dev/v1";
#[allow(dead_code)]
const FREEMODEL_ANTHROPIC_API: &str = "https://cc.freemodel.dev/v1";
const FREEMODEL_KEY_ENV: &str = "FREEMODEL_API_KEY";
const FREEMODEL_KEY_ATTEMPTS: usize = 20;

static FREEMODEL_KEY_INDEX: AtomicU64 = AtomicU64::new(0);

pub struct FreeModelProvider;

pub fn is_freemodel_model(model: &str) -> bool {
    model.starts_with("fm-")
}

pub async fn pool_stats() -> ProviderPoolStats {
    let key_count = load_keys().len();
    ProviderPoolStats {
        provider: "freemodel",
        ready: key_count,
        target: None,
        generated: None,
        failed: None,
        dead: None,
        cooling: None,
        degraded: key_count == 0,
        last_error: if key_count == 0 {
            Some("FREEMODEL_API_KEY is not configured".to_string())
        } else {
            None
        },
    }
}

impl CompletionProvider for FreeModelProvider {
    async fn stream_completion(
        &self,
        mut request: CompletionRequest,
    ) -> BoxStream<'static, Result<String>> {
        request.model = match freemodel_upstream_model(&request.model) {
            Some(model) => model.to_string(),
            None => {
                let model = request.model.clone();
                return Box::pin(stream::once(async move {
                    Err(anyhow!("unknown FreeModel model: {}", model))
                }));
            }
        };

        match freemodel_stream_completion(request).await {
            Ok(stream) => stream,
            Err(err) => Box::pin(stream::once(async move { Err(err) })),
        }
    }

    async fn complete_completion(&self, mut request: CompletionRequest) -> Result<String> {
        request.model = freemodel_upstream_model(&request.model)
            .ok_or_else(|| anyhow!("unknown FreeModel model: {}", request.model))?
            .to_string();
        let payload =
            json!({"model": request.model, "messages": request.messages, "stream": false});
        let response =
            freemodel_post_with_rotation(payload, request.proxy_url.as_deref(), false).await?;
        let data = response
            .json::<Value>()
            .await
            .context("FreeModel JSON response failed")?;
        extract_chat_content(&data).ok_or_else(|| anyhow!("FreeModel returned no assistant text"))
    }
}

pub fn freemodel_upstream_model(model: &str) -> Option<&str> {
    model.strip_prefix("fm-")
}

async fn freemodel_stream_completion(
    request: CompletionRequest,
) -> Result<BoxStream<'static, Result<String>>> {
    let payload = json!({"model": request.model, "messages": request.messages, "stream": true});
    let response =
        freemodel_post_with_rotation(payload, request.proxy_url.as_deref(), true).await?;
    Ok(Box::pin(stream_freemodel_sse_response(response)))
}

async fn freemodel_post_with_rotation(
    payload: Value,
    proxy_url: Option<&str>,
    stream: bool,
) -> Result<reqwest::Response> {
    let keys = load_keys();
    if keys.is_empty() {
        anyhow::bail!("FREEMODEL_API_KEY is not configured");
    }

    let attempts = FREEMODEL_KEY_ATTEMPTS.min(keys.len());
    let start = FREEMODEL_KEY_INDEX.fetch_add(1, Ordering::Relaxed) as usize;
    let mut last_error = None;

    for offset in 0..attempts {
        let key = &keys[(start + offset) % keys.len()];
        let client = build_client(proxy_url)?;
        let response = client
            .post(format!("{}/chat/completions", FREEMODEL_OPENAI_API))
            .bearer_auth(key)
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
            last_error = Some("FreeModel request network error".to_string());
            continue;
        };

        let status = response.status();
        if matches!(status.as_u16(), 401 | 403 | 429) {
            last_error = Some(format!("FreeModel key rejected with HTTP {}", status));
            continue;
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("FreeModel returned {}: {}", status, body);
        }
        return Ok(response);
    }

    anyhow::bail!(
        "all FreeModel keys failed; set FREEMODEL_API_KEY with one or more valid keys{}",
        last_error
            .map(|err| format!(" (last error: {})", err))
            .unwrap_or_default()
    )
}

fn build_client(proxy_url: Option<&str>) -> Result<Client> {
    let mut builder = Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent("leech-rs")
        .no_proxy();
    if let Some(url) = proxy_url {
        builder = builder.proxy(reqwest::Proxy::all(url)?);
    }
    Ok(builder.build()?)
}

fn stream_freemodel_sse_response(
    response: reqwest::Response,
) -> impl futures::Stream<Item = Result<String>> {
    stream::unfold(
        (response.bytes_stream(), String::new(), false),
        |(mut bytes, mut buffer, mut done)| async move {
            loop {
                if done {
                    return None;
                }
                if let Some((line, rest)) = take_line(&buffer) {
                    buffer = rest;
                    if let Some(delta) = parse_sse_line(&line) {
                        return Some((Ok(delta), (bytes, buffer, done)));
                    }
                    continue;
                }
                match bytes.next().await {
                    Some(Ok(chunk)) => buffer.push_str(&String::from_utf8_lossy(&chunk)),
                    Some(Err(err)) => {
                        done = true;
                        return Some((
                            Err(anyhow!("FreeModel stream read failed: {}", err)),
                            (bytes, buffer, done),
                        ));
                    }
                    None => {
                        done = true;
                        if !buffer.trim().is_empty() {
                            if let Some(delta) = parse_sse_line(&buffer) {
                                return Some((Ok(delta), (bytes, String::new(), done)));
                            }
                        }
                        return None;
                    }
                }
            }
        },
    )
}

fn take_line(buffer: &str) -> Option<(String, String)> {
    let idx = buffer.find('\n')?;
    Some((
        buffer[..idx].trim_end_matches('\r').to_string(),
        buffer[idx + 1..].to_string(),
    ))
}

fn parse_sse_line(line: &str) -> Option<String> {
    let line = line.trim();
    if !line.starts_with("data:") {
        return None;
    }
    let payload = line.trim_start_matches("data:").trim();
    if payload == "[DONE]" || payload.is_empty() {
        return None;
    }
    parse_sse_delta(payload)
}

fn load_keys() -> Vec<String> {
    std::env::var(FREEMODEL_KEY_ENV)
        .unwrap_or_default()
        .split([',', ';'])
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_public_prefix_for_upstream_model() {
        assert_eq!(
            freemodel_upstream_model("fm-openai/gpt-5.5"),
            Some("openai/gpt-5.5")
        );
        assert_eq!(
            freemodel_upstream_model("fm-qwen/qwen3-coder"),
            Some("qwen/qwen3-coder")
        );
        assert_eq!(freemodel_upstream_model("gpt-5-4"), None);
    }

    #[test]
    fn detects_freemodel_model_prefix() {
        assert!(is_freemodel_model("fm-openai/gpt-5.5"));
        assert!(!is_freemodel_model("gpt-5-4"));
    }
}
