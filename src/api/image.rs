//! Image upload and analysis endpoints.

use axum::{extract::State, response::Json, routing::post, Router};
use serde::Deserialize;
use serde_json::json;

use crate::account_pool::AccountPool;
use crate::direct::complete_completion;
use crate::models::resolve_model;
use crate::pool::acquire_direct_permit;

#[derive(Debug, Deserialize)]
pub struct ImageRequest {
    pub model: Option<String>,
    pub image: String,
    pub filename: Option<String>,
    pub question: Option<String>,
    pub stream: Option<bool>,
}

pub fn routes() -> Router<AccountPool> {
    Router::new()
        .route("/chat/with-image", post(handler))
        .route("/chat/upload-image", post(upload_handler))
}

async fn handler(
    State(pool): State<AccountPool>,
    Json(req): Json<ImageRequest>,
) -> Json<serde_json::Value> {
    let _permit = match acquire_direct_permit().await {
        Ok(p) => p,
        Err(e) => {
            return Json(json!({
                "error": format!("Concurrency limit: {}", e)
            }));
        }
    };

    let model = req.model.unwrap_or_else(|| "default".to_string());
    let model_slug = resolve_model(&model);

    let question = req.question.unwrap_or_else(|| "What's in this image?".to_string());
    let filename = req.filename.unwrap_or_else(|| "image.png".to_string());

    let messages = vec![
        serde_json::json!({
            "role": "user",
            "content": {
                "image": req.image,
                "text": question,
                "filename": filename,
            }
        })
    ];

    let account = match pool.acquire().await {
        Ok(acc) => acc,
        Err(e) => {
            return Json(json!({
                "error": format!("Failed to acquire account: {}", e)
            }));
        }
    };

    let proxy_url = pool.next_proxy().await;

    match complete_completion(&model_slug, &messages, proxy_url.as_deref(), account).await {
        Ok(reply) => Json(json!({
            "model": model_slug,
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": reply,
                }
            }]
        })),
        Err(e) => Json(json!({
            "error": format!("Analysis failed: {}", e)
        })),
    }
}

async fn upload_handler(State(_pool): State<AccountPool>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "stub",
        "analysis": "File upload not implemented in this version. Please use /chat/with-image with base64 data URI."
    }))
}
