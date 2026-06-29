//! API router aggregation.

pub mod chat;
pub mod messages;
pub mod models;
pub mod image;
pub mod health;
pub mod proxies;

use axum::{
    body::Body,
    extract::Request,
    middleware::{from_fn, Next},
    response::Response,
    Extension,
    Router,
};
use std::sync::Arc;

use crate::account_pool::AccountPool;
use crate::config::Config;
use crate::load_monitor::LoadMonitor;
use crate::tor_manager::TorManager;

async fn record_request(
    Extension(load_monitor): Extension<LoadMonitor>,
    req: Request<Body>,
    next: Next,
) -> Response {
    load_monitor.record_request().await;
    next.run(req).await
}

pub fn create_routes(
    pool: AccountPool,
    load_monitor: LoadMonitor,
    tor_manager: Arc<TorManager>,
    config: Config,
) -> Router {
    Router::new()
        .nest("/v1", chat::routes())
        .nest("/v1", messages::routes())
        .nest("/v1", models::routes())
        .nest("/v1", image::routes())
        .nest("/", health::routes())
        .nest("/", proxies::routes())
        .layer(from_fn(record_request))
        .layer(Extension(load_monitor))
        .layer(Extension(tor_manager))
        .layer(Extension(config))
        .with_state(pool)
}
