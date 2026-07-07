//! Sakana provider adapter.

use anyhow::Result;
use futures::stream::{self, BoxStream};
use futures::StreamExt;

use super::{CompletionProvider, CompletionRequest, ProviderPoolStats};
use crate::sakana;

pub struct SakanaProvider;

pub async fn pool_stats() -> ProviderPoolStats {
    let stats = sakana::pool_stats().await;
    ProviderPoolStats {
        provider: "sakana",
        ready: stats.ready,
        target: stats.target,
        generated: None,
        failed: None,
        dead: None,
        cooling: Some(stats.cooling),
        degraded: false,
        last_error: None,
    }
}

impl CompletionProvider for SakanaProvider {
    async fn stream_completion(
        &self,
        request: CompletionRequest,
    ) -> BoxStream<'static, Result<String>> {
        match sakana::stream_completion(
            &request.model,
            &request.messages,
            request.proxy_url.as_deref(),
        )
        .await
        {
            Ok(stream) => stream,
            Err(err) => Box::pin(stream::once(async move { Err(err) })),
        }
    }

    async fn complete_completion(&self, request: CompletionRequest) -> Result<String> {
        let mut stream = self.stream_completion(request).await;
        let mut output = String::new();
        while let Some(chunk) = stream.next().await {
            output.push_str(&chunk?);
        }

        if output.trim().is_empty() {
            anyhow::bail!("empty reply from Sakana");
        }

        Ok(output)
    }
}
