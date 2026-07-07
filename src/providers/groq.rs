//! Groq provider adapter.

use anyhow::{anyhow, Context, Result};
use futures::stream::{self, BoxStream};
use futures::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use super::openai_compat::{extract_chat_content, parse_sse_delta};
use super::{CompletionProvider, CompletionRequest, ProviderPoolStats};

const GROQ_API: &str = "https://api.groq.com/openai/v1";
const GROQ_KEY_ENV: &str = "GROQ_API_KEY";
const GROQ_KEY_ATTEMPTS: usize = 20;

static GROQ_KEY_INDEX: AtomicU64 = AtomicU64::new(0);

pub struct GroqProvider;

pub fn is_groq_model(model: &str) -> bool {
    model.starts_with("gr-")
}

pub async fn pool_stats() -> ProviderPoolStats {
    let key_count = load_keys().len();
    ProviderPoolStats {
        provider: "groq",
        ready: key_count,
        target: None,
        generated: None,
        failed: None,
        dead: None,
        cooling: None,
        degraded: key_count == 0,
        last_error: if key_count == 0 {
            Some("GROQ_API_KEY is not configured with any gsk_ keys".to_string())
        } else {
            None
        },
    }
}

impl CompletionProvider for GroqProvider {
    async fn stream_completion(
        &self,
        mut request: CompletionRequest,
    ) -> BoxStream<'static, Result<String>> {
        request.model = match groq_upstream_model(&request.model) {
            Some(model) => model.to_string(),
            None => {
                let model = request.model.clone();
                return Box::pin(stream::once(async move {
                    Err(anyhow!("unknown Groq model: {}", model))
                }));
            }
        };

        match groq_stream_completion(request).await {
            Ok(stream) => stream,
            Err(err) => Box::pin(stream::once(async move { Err(err) })),
        }
    }

    async fn complete_completion(&self, mut request: CompletionRequest) -> Result<String> {
        request.model = groq_upstream_model(&request.model)
            .ok_or_else(|| anyhow!("unknown Groq model: {}", request.model))?
            .to_string();
        let payload =
            json!({"model": request.model, "messages": request.messages, "stream": false});
        let response =
            groq_post_with_rotation(payload, request.proxy_url.as_deref(), false).await?;
        let data = response
            .json::<Value>()
            .await
            .context("Groq JSON response failed")?;
        extract_chat_content(&data).ok_or_else(|| anyhow!("Groq returned no assistant text"))
    }
}

pub fn groq_upstream_model(model: &str) -> Option<&'static str> {
    match model {
        "gr-llama-8b" => Some("llama-3.1-8b-instant"),
        "gr-gemma-9b" => Some("gemma2-9b-it"),
        "gr-mixtral-8x7b" => Some("mixtral-8x7b-32768"),
        "gr-llama-70b" => Some("llama-3.3-70b-versatile"),
        "gr-deepseek-r1" => Some("deepseek-r1-distill-llama-70b"),
        "gr-qwen-32b" => Some("qwen-qwq-32b"),
        "gr-llama-4-scout" => Some("meta-llama/llama-4-scout-17b-16e-instruct"),
        "gr-kimi-k2" => Some("moonshotai/kimi-k2-instruct"),
        "gr-qwen3-32b" => Some("qwen/qwen3-32b"),
        "gr-compound-beta" => Some("compound-beta"),
        _ => None,
    }
}

async fn groq_stream_completion(
    request: CompletionRequest,
) -> Result<BoxStream<'static, Result<String>>> {
    let payload = json!({"model": request.model, "messages": request.messages, "stream": true});
    let response = groq_post_with_rotation(payload, request.proxy_url.as_deref(), true).await?;
    Ok(Box::pin(stream_groq_sse_response(response)))
}

async fn groq_post_with_rotation(
    payload: Value,
    proxy_url: Option<&str>,
    stream: bool,
) -> Result<reqwest::Response> {
    let keys = load_keys();
    if keys.is_empty() {
        anyhow::bail!("GROQ_API_KEY is not configured with any valid gsk_ keys");
    }

    let attempts = GROQ_KEY_ATTEMPTS.min(keys.len());
    let start = GROQ_KEY_INDEX.fetch_add(1, Ordering::Relaxed) as usize;
    let mut last_error = None;

    for offset in 0..attempts {
        let key = &keys[(start + offset) % keys.len()];
        let client = build_client(proxy_url)?;
        let response = client
            .post(format!("{}/chat/completions", GROQ_API))
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
            last_error = Some("Groq request network error".to_string());
            continue;
        };

        let status = response.status();
        if matches!(status.as_u16(), 401 | 403 | 429) {
            last_error = Some(format!("Groq key rejected with HTTP {}", status));
            continue;
        }
        if status.as_u16() == 503 {
            anyhow::bail!("Groq model is temporarily unavailable: HTTP 503");
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Groq returned {}: {}", status, body);
        }
        return Ok(response);
    }

    anyhow::bail!(
        "all Groq keys failed; set GROQ_API_KEY with one or more valid gsk_ keys{}",
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

fn stream_groq_sse_response(
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
                            Err(anyhow!("Groq stream read failed: {}", err)),
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
    std::env::var(GROQ_KEY_ENV)
        .unwrap_or_default()
        .split([',', ';'])
        .map(str::trim)
        .filter(|key| key.starts_with("gsk_"))
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_public_models_to_groq_upstream_models() {
        assert_eq!(
            groq_upstream_model("gr-llama-8b"),
            Some("llama-3.1-8b-instant")
        );
        assert_eq!(groq_upstream_model("gr-qwen3-32b"), Some("qwen/qwen3-32b"));
        assert_eq!(groq_upstream_model("gpt-5-4"), None);
    }

    #[test]
    fn detects_groq_model_prefix() {
        assert!(is_groq_model("gr-llama-8b"));
        assert!(!is_groq_model("gpt-5-4"));
    }
}
