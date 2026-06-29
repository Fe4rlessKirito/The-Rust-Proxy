//! Image upload and analysis endpoints.

use axum::{extract::{Multipart, State}, response::Json, routing::post, Router};
use base64::{engine::general_purpose, Engine as _};
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

async fn upload_handler(
    State(pool): State<AccountPool>,
    mut multipart: Multipart,
) -> Json<serde_json::Value> {
    let _permit = match acquire_direct_permit().await {
        Ok(p) => p,
        Err(e) => {
            return Json(json!({
                "error": format!("Concurrency limit: {}", e)
            }));
        }
    };

    let mut file_bytes = None;
    let mut filename = "image.png".to_string();
    let mut question = "What's in this image?".to_string();
    let mut model = "gpt-5-4".to_string();

    while let Ok(Some(field)) = multipart.next_field().await {
        let name = field.name().unwrap_or_default().to_string();
        match name.as_str() {
            "file" => {
                filename = field.file_name().unwrap_or("image.png").to_string();
                match field.bytes().await {
                    Ok(bytes) => file_bytes = Some(bytes.to_vec()),
                    Err(e) => {
                        return Json(json!({
                            "error": format!("Failed to read uploaded file: {}", e)
                        }));
                    }
                }
            }
            "question" => {
                if let Ok(text) = field.text().await {
                    question = text;
                }
            }
            "model" => {
                if let Ok(text) = field.text().await {
                    model = text;
                }
            }
            _ => {}
        }
    }

    let Some(file_bytes) = file_bytes else {
        return Json(json!({
            "error": "Missing multipart field 'file'"
        }));
    };

    let image = format!(
        "data:image/png;base64,{}",
        general_purpose::STANDARD.encode(file_bytes)
    );
    let model_slug = resolve_model(&model);
    let messages = vec![serde_json::json!({
        "role": "user",
        "content": {
            "image": image,
            "text": question,
            "filename": filename,
        }
    })];

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
            "analysis": reply,
        })),
        Err(e) => Json(json!({
            "error": format!("Analysis failed: {}", e)
        })),
    }
}
