//! API router aggregation.

pub mod chat;
mod format;
pub mod health;
pub mod messages;
pub mod models;
pub mod proxies;
mod tools;
pub mod usage;

use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    middleware::{from_fn, Next},
    response::{IntoResponse, Response},
    routing::get_service,
    Extension, Router,
};
use std::sync::Arc;
use tower_http::services::{ServeDir, ServeFile};

use crate::account_pool::AccountPool;
use crate::config::Config;
use crate::load_monitor::LoadMonitor;

async fn record_request(
    Extension(load_monitor): Extension<LoadMonitor>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let method = req.method().as_str();
    if method == "POST" && matches!(path, "/v1/chat/completions" | "/v1/messages") {
        load_monitor.record_request().await;
    }
    next.run(req).await
}

async fn removed_config_handler() -> Response {
    (StatusCode::NOT_FOUND, "config endpoint removed").into_response()
}

pub fn create_routes(
    pool: AccountPool,
    load_monitor: LoadMonitor,
    config: Config,
) -> Router {
    let dashboard_dir = std::path::PathBuf::from("frontend").join("dist");
    let dashboard_index = dashboard_dir.join("index.html");
    let dashboard_service = get_dashboard_service(dashboard_dir, dashboard_index.clone());

    Router::new()
        .nest("/v1", chat::routes())
        .nest("/v1", messages::routes())
        .nest("/v1", models::routes())
        .nest("/", health::routes())
        .nest("/", proxies::routes())
        .nest("/", usage::routes())
        .route("/", get_service(ServeFile::new(dashboard_index.clone())))
        .route("/index.html", get_service(ServeFile::new(dashboard_index)))
        .route("/config", axum::routing::get(removed_config_handler))
        .fallback_service(dashboard_service)
        .layer(from_fn(record_request))
        .layer(Extension(load_monitor))
        .layer(Extension(config))
        .with_state(pool)
}

fn get_dashboard_service(
    dashboard_dir: std::path::PathBuf,
    dashboard_index: std::path::PathBuf,
) -> axum::routing::MethodRouter {
    let index_service = ServeFile::new(dashboard_index);
    axum::routing::get_service(
        ServeDir::new(dashboard_dir)
            .not_found_service(index_service.clone())
            .fallback(index_service),
    )
}

#[cfg(test)]
mod tests {
    use super::create_routes;
    use crate::account_pool::AccountPool;
    use crate::config::Config;
    use crate::load_monitor::LoadMonitor;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    async fn app() -> axum::Router {
        let pool = AccountPool::new(1, std::time::Duration::from_secs(60), 5, 10).await;

        create_routes(pool, LoadMonitor::new(), Config::default())
    }

    #[tokio::test]
    async fn public_routes_expose_usage_and_hide_config() {
        let app = app().await;

        let health = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let usage = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/usage/overview")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(usage.status(), StatusCode::OK);

        let dashboard = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(dashboard.status(), StatusCode::OK);
        let body = to_bytes(dashboard.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains(r#"src="./main.js""#));

        let dashboard_js = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/main.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(dashboard_js.status(), StatusCode::OK);
        let body = to_bytes(dashboard_js.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("renderShell"));

        let config = app
            .oneshot(
                Request::builder()
                    .uri("/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(config.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn chat_completions_rejects_missing_required_fields() {
        let app = app().await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"gpt-5-4"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn messages_rejects_missing_required_fields() {
        let app = app().await;
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"model":"claude-sonnet-4-6"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
