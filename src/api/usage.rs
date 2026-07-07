//! Usage and session accounting endpoints.

use axum::{
    extract::{Path, State},
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::account_pool::AccountPool;
use crate::usage;

#[derive(Debug, Deserialize)]
struct CapRequest {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    cap: Option<u64>,
}

pub fn routes() -> Router<AccountPool> {
    Router::new()
        .route("/usage/overview", get(overview_handler))
        .route("/usage/session/:session_id", get(session_handler))
        .route("/usage/cap", post(cap_handler))
        .route("/usage/reset", post(reset_handler))
}

async fn overview_handler(State(_pool): State<AccountPool>) -> Json<serde_json::Value> {
    Json(json!(usage::overview()))
}

async fn session_handler(
    State(_pool): State<AccountPool>,
    Path(session_id): Path<String>,
) -> Json<serde_json::Value> {
    Json(json!(usage::session_snapshot(&session_id)))
}

async fn cap_handler(
    State(_pool): State<AccountPool>,
    Json(req): Json<CapRequest>,
) -> Json<serde_json::Value> {
    let session_id = req.session_id.unwrap_or_else(|| "default".to_string());
    match usage::set_cap(&session_id, req.cap) {
        Ok(snapshot) => Json(json!(snapshot)),
        Err(err) => Json(json!({ "error": format!("Failed to update cap: {}", err) })),
    }
}

async fn reset_handler(State(_pool): State<AccountPool>) -> Json<serde_json::Value> {
    match usage::reset_all() {
        Ok(()) => Json(json!({ "ok": true })),
        Err(err) => Json(json!({ "error": format!("Failed to reset usage: {}", err) })),
    }
}
