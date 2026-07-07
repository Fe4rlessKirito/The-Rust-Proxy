use anyhow::Result;
use futures::stream::BoxStream;
use serde_json::Value;

use crate::account_pool::Account;

#[derive(Clone, Debug, serde::Serialize)]
pub struct ProviderPoolStats {
    pub provider: &'static str,
    pub ready: usize,
    pub target: Option<usize>,
    pub generated: Option<u64>,
    pub failed: Option<u64>,
    pub dead: Option<u64>,
    pub cooling: Option<usize>,
    pub degraded: bool,
    pub last_error: Option<String>,
}

#[derive(Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<Value>,
    pub proxy_url: Option<String>,
    pub account: Option<Account>,
}

pub trait CompletionProvider {
    fn stream_completion(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<Output = BoxStream<'static, Result<String>>> + Send;

    fn complete_completion(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<String>> + Send;
}
