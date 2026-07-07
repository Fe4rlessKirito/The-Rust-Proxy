//! use.ai-compatible provider adapter.

use anyhow::{anyhow, Result};
use futures::stream::{self, BoxStream};
use futures::StreamExt;

use super::{CompletionProvider, CompletionRequest};
use crate::direct;

pub struct UseAiProvider;

impl CompletionProvider for UseAiProvider {
    async fn stream_completion(
        &self,
        request: CompletionRequest,
    ) -> BoxStream<'static, Result<String>> {
        let Some(account) = request.account else {
            let err = anyhow!("use.ai provider requires an account");
            return Box::pin(stream::once(async move { Err(err) }));
        };

        direct::stream_completion(
            &request.model,
            &request.messages,
            request.proxy_url.as_deref(),
            account,
        )
        .await
    }

    async fn complete_completion(&self, request: CompletionRequest) -> Result<String> {
        let mut stream = self.stream_completion(request).await;
        let mut parts = Vec::new();
        while let Some(item) = stream.next().await {
            parts.push(item?);
        }
        let reply = parts.concat();
        if reply.is_empty() {
            anyhow::bail!("empty reply");
        }
        Ok(reply)
    }
}
