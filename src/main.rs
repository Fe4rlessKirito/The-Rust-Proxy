//! Leech-RS - a high-performance, headless LLM proxy for use.ai.

mod config;
mod account_pool;
mod direct;
mod filter;
mod models;
mod utils;
mod api;
mod pool;
mod tor_manager;
mod load_monitor;
mod scale_controller;

use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};
use tracing_subscriber;

use load_monitor::LoadMonitor;
use scale_controller::ScaleController;
use tor_manager::TorManager;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("debug")
        .init();

    let cfg = config::Config::load().unwrap_or_else(|e| {
        eprintln!("Failed to load config, using defaults: {}", e);
        config::Config::default()
    });

    info!("Starting Leech-RS with config: {:?}", cfg);

    let base_port = cfg.proxy.tor_ports.first().copied().unwrap_or(9050);
    let tor_manager = Arc::new(TorManager::new(base_port));

    let initial_ports = if cfg.proxy.tor_ports.is_empty() {
        (0..cfg.proxy.tor_instances.max(1))
            .map(|idx| base_port.saturating_add(idx as u16))
            .collect::<Vec<_>>()
    } else {
        cfg.proxy.tor_ports.clone()
    };

    for port in initial_ports {
        match tor_manager.add_existing_or_spawn(port).await {
            Ok(url) => info!("Registered initial Tor proxy: {}", url),
            Err(e) => warn!("Failed to start Tor on port {}: {}", port, e),
        }
    }

    if let Some(url) = &cfg.proxy.socks5_url {
        if !url.is_empty() && tor_manager.get_proxies().await.is_empty() {
            warn!("socks5_url fallback is configured but dynamic TorManager only manages Tor ports");
        }
    }

    let load_monitor = LoadMonitor::new();

    let pool = account_pool::AccountPool::new(
        cfg.account_pool.size,
        Duration::from_secs(cfg.account_pool.ttl_sec),
        tor_manager.clone(),
        cfg.account_pool.refill_sec,
        cfg.account_pool.signup_delay_ms,
    )
    .await;
    pool.start().await;

    let scale_controller = ScaleController::new(
        tor_manager.clone(),
        load_monitor.clone(),
        pool.clone(),
        cfg.account_pool.size,
        cfg.proxy.tor_instances.max(1),
        cfg.proxy.tor_instances.max(10),
        5.0,
        1.0,
        Duration::from_secs(30),
    );
    tokio::spawn(async move {
        scale_controller.run().await;
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = api::create_routes(pool.clone(), load_monitor, tor_manager.clone()).layer(cors);

    let addr = SocketAddr::from(([127, 0, 0, 1], cfg.server.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Server listening on http://{}", addr);

    let server = axum::serve(listener, app);

    tokio::select! {
        result = server => {
            result?;
        }
        _ = shutdown_signal() => {
            info!("Shutting down gracefully...");
        }
    }

    pool.stop().await;
    let _ = tor_manager.stop_all().await;

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            warn!("Failed to install Ctrl+C handler: {}", e);
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(e) => {
                warn!("Failed to install SIGTERM handler: {}", e);
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
