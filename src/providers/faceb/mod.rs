//! Faceb.ai provider adapter.

mod account;
mod http;
mod pool;
mod sse;

use anyhow::{anyhow, Context, Result};
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{json, Value};

use super::openai_compat::extract_chat_content;
use super::{CompletionProvider, CompletionRequest, ProviderPoolStats};

const FACEB_BASE: &str = "https://faceb.ai";
const FACEB_API: &str = "https://api.faceb.ai/v1";
const FACEB_POOL_BATCH: usize = 4;
const FACEB_POOL_MAX: usize = 256;
const FACEB_KEY_ATTEMPTS: usize = 50;
const FACEB_POOL_WAIT_MS: u64 = 500;
const FACEB_POOL_FILE: &str = "faceb_pool.json";
const FACEB_WARM_TARGET: usize = 16;
const FACEB_REFILL_INTERVAL_SEC: u64 = 15;

pub struct FacebProvider;

#[derive(Debug, serde::Serialize)]
pub struct FacebPoolSnapshot {
    pub pool_size: usize,
    pub gen_total: u64,
    pub fail_total: u64,
    pub dead_keys: u64,
    pub last_error: Option<String>,
}

pub fn is_faceb_model(model: &str) -> bool {
    model.starts_with("faceb-")
}

pub async fn pool_stats() -> ProviderPoolStats {
    pool::pool_stats().await
}

pub async fn pool_snapshot() -> FacebPoolSnapshot {
    pool::pool_snapshot().await
}

fn faceb_upstream_model(model: &str) -> &str {
    model.strip_prefix("faceb-").unwrap_or(model)
}

impl CompletionProvider for FacebProvider {
    async fn stream_completion(
        &self,
        mut request: CompletionRequest,
    ) -> BoxStream<'static, Result<String>> {
        request.model = faceb_upstream_model(&request.model).to_string();
        let stream = async_stream::stream! {
            match http::faceb_stream_completion(request).await {
                Ok(mut upstream) => {
                    while let Some(item) = upstream.next().await {
                        yield item;
                    }
                }
                Err(err) => yield Err(err),
            }
        };
        Box::pin(stream)
    }

    async fn complete_completion(&self, mut request: CompletionRequest) -> Result<String> {
        request.model = faceb_upstream_model(&request.model).to_string();
        let payload =
            json!({"model": request.model, "messages": request.messages, "stream": false});
        let response =
            http::faceb_post_with_rotation(payload, request.proxy_url.as_deref(), false).await?;
        let data = response
            .json::<Value>()
            .await
            .context("Faceb JSON response failed")?;
        if http::is_dead_response(&data) {
            pool::mark_dead();
            anyhow::bail!("Faceb key exhausted")
        }
        extract_chat_content(&data).ok_or_else(|| anyhow!("Faceb returned no assistant text"))
    }
}

pub async fn start_background_warmup(proxies: Vec<String>) {
    pool::start_background_warmup(proxies).await;
}

pub async fn set_proxies(proxies: Vec<String>) {
    pool::set_proxies(proxies).await;
}

pub async fn stop_background_warmup() {
    pool::stop_background_warmup().await;
}
