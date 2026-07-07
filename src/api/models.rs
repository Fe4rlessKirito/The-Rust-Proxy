//! /v1/models endpoint.

use crate::account_pool::AccountPool;
use axum::{extract::State, response::Json, routing::get, Router};

pub fn routes() -> Router<AccountPool> {
    Router::new().route("/models", get(handler))
}

async fn handler(State(_pool): State<AccountPool>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "object": "list",
        "data": crate::models::model_list()
    }))
}
