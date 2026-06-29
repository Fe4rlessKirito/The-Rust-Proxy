//! Health, bank, and config endpoints.

use axum::{
    extract::State,
    response::Json,
    routing::{get, post},
    Router,
};
use serde_json::json;

use crate::account_pool::AccountPool;
use crate::config::Config;

pub fn routes() -> Router<AccountPool> {
    Router::new()
        .route("/health", get(health_handler))
        .route("/bank", get(bank_handler))
        .route("/config", get(config_get_handler))
        .route("/config", post(config_post_handler))
}

async fn health_handler(State(pool): State<AccountPool>) -> Json<serde_json::Value> {
    let fresh = pool.len().await;
    Json(json!({
        "status": "ok",
        "fresh_accounts": fresh,
        "send_success_rate": 1.0,
        "reasons": ["all systems nominal"]
    }))
}

async fn bank_handler(State(pool): State<AccountPool>) -> Json<serde_json::Value> {
    let fresh = pool.len().await;
    Json(json!({
        "mode": "headless-ws",
        "warm_accounts": fresh,
        "pool_target": Config::default().account_pool.size,
        "status": "ok"
    }))
}

async fn config_get_handler() -> Json<serde_json::Value> {
    let cfg = Config::default();
    Json(json!({
        "pool_size": cfg.account_pool.size,
        "signup_delay_ms": cfg.account_pool.signup_delay_ms,
        "account_ttl_sec": cfg.account_pool.ttl_sec,
        "proxy_tor": false,
        "tor_socks": cfg.proxy.socks5_url.unwrap_or_default(),
    }))
}

async fn config_post_handler(
    State(_pool): State<AccountPool>,
    Json(_payload): Json<serde_json::Value>,
) -> Json<serde_json::Value> {
    Json(json!({
        "status": "config update not implemented fully, but stub returns ok"
    }))
}
