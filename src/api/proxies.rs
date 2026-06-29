//! Dynamic proxy and load status endpoint.

use axum::{extract::Extension, response::Json, routing::get, Router};
use serde_json::json;
use std::sync::Arc;

use crate::account_pool::AccountPool;
use crate::load_monitor::LoadMonitor;
use crate::tor_manager::TorManager;

pub fn routes() -> Router<AccountPool> {
    Router::new().route("/proxies", get(proxies_handler))
}

async fn proxies_handler(
    Extension(tor_manager): Extension<Arc<TorManager>>,
    Extension(load_monitor): Extension<LoadMonitor>,
) -> Json<serde_json::Value> {
    let proxies = tor_manager.get_proxies().await;
    let (window_requests, requests_per_second) = load_monitor.snapshot().await;

    Json(json!({
        "proxies": proxies,
        "proxy_count": proxies.len(),
        "load": {
            "window_requests": window_requests,
            "requests_per_second": requests_per_second,
        }
    }))
}
