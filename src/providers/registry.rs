use anyhow::Result;
use futures::stream::BoxStream;

use crate::account_pool::AccountPool;

use super::types::{CompletionProvider, CompletionRequest, ProviderPoolStats};
use super::{faceb, sakana, use_ai};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ProviderId {
    UseAi,
    Sakana,
    Faceb,
}

impl ProviderId {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::UseAi => "use_ai",
            ProviderId::Sakana => "sakana",
            ProviderId::Faceb => "faceb",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccountPolicy {
    None,
    UseAiAccount,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProxyPolicy {
    None,
    UseAiPool,
    ProviderRoundRobin,
}

#[derive(Clone, Copy, Debug)]
pub struct ProviderSpec {
    pub id: ProviderId,
    pub account_policy: AccountPolicy,
    pub proxy_policy: ProxyPolicy,
}

pub fn provider_id_for_model(model: &str) -> ProviderId {
    if faceb::is_faceb_model(model) {
        ProviderId::Faceb
    } else if crate::sakana::is_sakana_model(model) {
        ProviderId::Sakana
    } else {
        ProviderId::UseAi
    }
}

pub fn provider_spec_for_model(model: &str) -> ProviderSpec {
    match provider_id_for_model(model) {
        ProviderId::UseAi => ProviderSpec {
            id: ProviderId::UseAi,
            account_policy: AccountPolicy::UseAiAccount,
            proxy_policy: ProxyPolicy::UseAiPool,
        },
        ProviderId::Sakana => ProviderSpec {
            id: ProviderId::Sakana,
            account_policy: AccountPolicy::None,
            proxy_policy: ProxyPolicy::None,
        },
        ProviderId::Faceb => ProviderSpec {
            id: ProviderId::Faceb,
            account_policy: AccountPolicy::None,
            proxy_policy: ProxyPolicy::ProviderRoundRobin,
        },
    }
}

pub fn requires_use_ai_account(model: &str) -> bool {
    provider_spec_for_model(model).account_policy == AccountPolicy::UseAiAccount
}

pub async fn proxy_url_for_model(model: &str, pool: &AccountPool) -> Option<String> {
    let spec = provider_spec_for_model(model);
    match spec.proxy_policy {
        ProxyPolicy::UseAiPool => pool.next_proxy().await,
        ProxyPolicy::ProviderRoundRobin => {
            crate::provider_proxies::next_proxy(spec.id.as_str()).await
        }
        ProxyPolicy::None => None,
    }
}

pub async fn stream_completion(request: CompletionRequest) -> BoxStream<'static, Result<String>> {
    match provider_id_for_model(&request.model) {
        ProviderId::Faceb => faceb::FacebProvider.stream_completion(request).await,
        ProviderId::Sakana => sakana::SakanaProvider.stream_completion(request).await,
        ProviderId::UseAi => use_ai::UseAiProvider.stream_completion(request).await,
    }
}

pub async fn complete_completion(request: CompletionRequest) -> Result<String> {
    match provider_id_for_model(&request.model) {
        ProviderId::Faceb => faceb::FacebProvider.complete_completion(request).await,
        ProviderId::Sakana => sakana::SakanaProvider.complete_completion(request).await,
        ProviderId::UseAi => use_ai::UseAiProvider.complete_completion(request).await,
    }
}

pub async fn pool_stats(use_ai_ready: usize, use_ai_target: usize) -> Vec<ProviderPoolStats> {
    let mut stats = vec![ProviderPoolStats {
        provider: ProviderId::UseAi.as_str(),
        ready: use_ai_ready,
        target: Some(use_ai_target),
        generated: None,
        failed: None,
        dead: None,
        cooling: None,
        degraded: use_ai_ready == 0,
        last_error: if use_ai_ready == 0 {
            Some("use.ai account pool is empty".to_string())
        } else {
            None
        },
    }];
    stats.push(sakana::pool_stats().await);
    stats.push(faceb::pool_stats().await);
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_faceb_prefixed_models_to_faceb() {
        assert_eq!(
            provider_id_for_model("faceb-openai/gpt-5").as_str(),
            "faceb"
        );
        assert!(!requires_use_ai_account("faceb-openai/gpt-5"));
    }

    #[test]
    fn routes_sakana_models_to_sakana() {
        assert_eq!(provider_id_for_model("sakana-fugu").as_str(), "sakana");
        assert!(!requires_use_ai_account("sakana-fugu"));
    }

    #[test]
    fn routes_freemodel_models_to_use_ai() {
        assert_eq!(provider_id_for_model("fm-openai/gpt-5.5").as_str(), "use_ai");
        assert!(requires_use_ai_account("fm-openai/gpt-5.5"));
        assert_eq!(
            provider_spec_for_model("fm-openai/gpt-5.5").proxy_policy,
            ProxyPolicy::UseAiPool
        );
    }

    #[test]
    fn routes_groq_models_to_use_ai() {
        assert_eq!(provider_id_for_model("gr-llama-8b").as_str(), "use_ai");
        assert!(requires_use_ai_account("gr-llama-8b"));
        assert_eq!(
            provider_spec_for_model("gr-llama-8b").proxy_policy,
            ProxyPolicy::UseAiPool
        );
    }

    #[test]
    fn routes_default_models_to_use_ai() {
        assert_eq!(provider_id_for_model("gpt-5-4").as_str(), "use_ai");
        assert!(requires_use_ai_account("gpt-5-4"));
    }
}
