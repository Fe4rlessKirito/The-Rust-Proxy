//! Proxy and load status endpoint.
//!
//! The gateway is direct-egress only (no Tor). `/proxies` still reports load
//! and the configured provider route counts for dashboard compatibility, but
//! there are no active SOCKS5 proxies to expose.

use axum::{extract::Extension, response::Json, routing::get, Router};
use serde_json::json;

use crate::account_pool::AccountPool;
use crate::config::{Config, ProviderProxyConfig};
use crate::load_monitor::LoadMonitor;

pub fn routes() -> Router<AccountPool> {
    Router::new().route("/proxies", get(proxies_handler))
}

async fn proxies_handler(
    Extension(load_monitor): Extension<LoadMonitor>,
    Extension(config): Extension<Config>,
) -> Json<serde_json::Value> {
    let provider_assignments = crate::provider_proxies::assignments().await;
    let provider_configured_routes = configured_route_counts(&config.provider_proxies);
    let (window_requests, requests_per_second) = load_monitor.snapshot().await;
    let requests_per_minute = requests_per_second * 60.0;

    Json(json!({
        "proxies": Vec::<String>::new(),
        "proxy_count": 0,
        "egress": "direct",
        "provider_assignments": provider_assignments,
        "provider_configured_routes": provider_configured_routes,
        "load": {
            "window_requests": window_requests,
            "requests_per_minute": requests_per_minute,
        }
    }))
}

fn configured_route_counts(config: &ProviderProxyConfig) -> serde_json::Value {
    json!({
        "use_ai": config.use_ai_ports.len(),
        "sakana": config.sakana_ports.len(),
        "faceb": config.faceb_ports.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_configured_provider_route_counts() {
        let config = ProviderProxyConfig {
            use_ai_ports: vec![9050, 9051],
            sakana_ports: vec![],
            faceb_ports: vec![9071],
        };

        assert_eq!(
            configured_route_counts(&config),
            json!({
                "use_ai": 2,
                "sakana": 0,
                "faceb": 1,
            })
        );
    }
}
