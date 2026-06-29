//! /v1/models endpoint.

use axum::{extract::State, response::Json, routing::get, Router};
use crate::account_pool::AccountPool;

pub fn routes() -> Router<AccountPool> {
    Router::new().route("/models", get(handler))
}

async fn handler(State(_pool): State<AccountPool>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "object": "list",
        "data": crate::models::model_list()
    }))
}
