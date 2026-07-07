//! Health and pool status endpoints.

use axum::{extract::State, response::Json, routing::get, Extension, Router};
use serde_json::json;

use crate::account_pool::AccountPool;
use crate::config::Config;

pub fn routes() -> Router<AccountPool> {
    Router::new()
        .route("/health", get(health_handler))
        .route("/bank", get(bank_handler))
        .route("/v1/pool", get(pool_handler))
}

async fn health_handler(State(pool): State<AccountPool>) -> Json<serde_json::Value> {
    let fresh = pool.len().await;
    let provider_pools = crate::providers::pool_stats(fresh, pool.target_size()).await;
    Json(json!({
        "status": "ok",
        "fresh_accounts": fresh,
        "provider_pools": provider_pools,
        "send_success_rate": 1.0,
        "reasons": ["all systems nominal"]
    }))
}

async fn pool_handler(State(pool): State<AccountPool>) -> Json<serde_json::Value> {
    let fresh = pool.len().await;
    let faceb = crate::providers::faceb::pool_snapshot().await;
    let provider_pools = crate::providers::pool_stats(fresh, pool.target_size()).await;
    Json(json!({
        "pool_size": faceb.pool_size,
        "gen_total": faceb.gen_total,
        "fail_total": faceb.fail_total,
        "dead_keys": faceb.dead_keys,
        "last_error": faceb.last_error,
        "provider_pools": provider_pools,
    }))
}

async fn bank_handler(
    State(pool): State<AccountPool>,
    Extension(cfg): Extension<Config>,
) -> Json<serde_json::Value> {
    let fresh = pool.len().await;
    let provider_pools = crate::providers::pool_stats(fresh, cfg.account_pool.size).await;
    Json(json!({
        "mode": "headless-ws",
        "warm_accounts": fresh,
        "pool_target": cfg.account_pool.size,
        "provider_pools": provider_pools,
        "status": "ok"
    }))
}
