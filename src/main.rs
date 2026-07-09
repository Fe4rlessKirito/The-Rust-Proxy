//! Leech-RS - a high-performance, headless LLM proxy for use.ai.

mod account_pool;
mod api;
mod config;
mod direct;
mod filter;
mod load_monitor;
mod models;
mod pool;
mod provider_proxies;
mod providers;
mod sakana;
mod scale_controller;
mod temp_mail;
mod tor_manager;
mod usage;
mod utils;

use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use load_monitor::LoadMonitor;
use scale_controller::ScaleController;
use tor_manager::TorManager;

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = config::Config::load()?;
    init_logging(&cfg);

    usage::init().unwrap_or_else(|e| {
        eprintln!("Failed to init usage metering: {}", e);
    });

    info!(
        "Starting Leech-RS on {}:{}",
        cfg.server.host, cfg.server.port
    );
    log_startup_diagnostics(&cfg);

    let tor_manager = Arc::new(TorManager::new(9050));

    let initial_ports = configured_initial_tor_ports(&cfg);

    for port in initial_ports {
        match tor_manager.add_existing_or_spawn(port).await {
            Ok(url) => info!("Registered initial Tor proxy: {}", url),
            Err(e) => warn!("Failed to start Tor on port {}: {}", port, e),
        }
    }

    let active_proxies = tor_manager.get_proxies().await;
    provider_proxies::sync_active(&active_proxies, &cfg.provider_proxies).await;
    let use_ai_proxies = provider_proxies::assigned("use_ai").await;
    let faceb_proxies = provider_proxies::assigned("faceb").await;

    if let Some(url) = &cfg.proxy.socks5_url {
        if !url.is_empty() && tor_manager.get_proxies().await.is_empty() {
            warn!(
                "socks5_url fallback is configured but dynamic TorManager only manages Tor ports"
            );
        }
    }

    let load_monitor = LoadMonitor::new();

    let pool = account_pool::AccountPool::new_with_proxies(
        cfg.account_pool.size,
        Duration::from_secs(cfg.account_pool.ttl_sec),
        tor_manager.clone(),
        use_ai_proxies,
        cfg.account_pool.refill_sec,
        cfg.account_pool.signup_delay_ms,
    )
    .await;
    pool.start().await;
    providers::faceb::start_background_warmup(faceb_proxies).await;

    let scale_controller = ScaleController::new(
        tor_manager.clone(),
        load_monitor.clone(),
        pool.clone(),
        cfg.account_pool.size,
        cfg.proxy
            .tor_instances
            .max(1)
            .min(cfg.provider_proxies.use_ai_ports.len().max(1)),
        cfg.provider_proxies.use_ai_ports.len().max(1),
        cfg.provider_proxies.use_ai_ports.clone(),
        5.0,
        1.0,
        Duration::from_secs(30),
    );
    let scale_controller = Arc::new(scale_controller);
    let scale_runner = scale_controller.clone();
    let scale_handle = tokio::spawn(async move {
        scale_runner.run().await;
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = api::create_routes(pool.clone(), load_monitor, tor_manager.clone(), cfg.clone())
        .layer(cors);

    let addr = format!("{}:{}", cfg.server.host, cfg.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let local_addr: SocketAddr = listener.local_addr()?;
    info!("Server listening on http://{}", local_addr);

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            shutdown_signal().await;
            info!("Shutting down gracefully...");
        })
        .await?;

    pool.stop().await;
    providers::faceb::stop_background_warmup().await;
    scale_controller.stop();
    if tokio::time::timeout(Duration::from_secs(6), scale_handle)
        .await
        .is_err()
    {
        warn!("Scale controller did not stop within timeout");
    }
    let _ = tor_manager.stop_all().await;

    Ok(())
}

fn init_logging(cfg: &config::Config) {
    let base = std::env::var("LEECH_LOG")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| cfg.logging.level.clone());

    // Suppress the chattiest third-party crates regardless of the base level.
    // The worst offender is `html5ever` (pulled in by `scraper`, used for
    // Faceb account provisioning): it logs every parsed HTML token at DEBUG,
    // which floods log sinks past rate limits (e.g. Railway's 500 logs/sec).
    // `selectors` is the same story. The network crates are capped at `info`
    // so their warnings/errors still surface while their debug noise does
    // not. These target-specific directives override a bare level directive.
    let filter_str = format!(
        "{base},\
         html5ever=off,\
         html5ever::tree_builder=off,\
         selectors=off,\
         scraper=off,\
         cookie_store=info,\
         hyper=info,\
         hyper_util=info,\
         reqwest=info,\
         h2=info,\
         rustls=info,\
         tungstenite=info,\
         mio=info"
    );

    let filter = EnvFilter::try_new(&filter_str).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn log_startup_diagnostics(cfg: &config::Config) {
    info!("Model endpoints:");
    info!("  POST /v1/chat/completions");
    info!("  POST /v1/messages");
    info!("Operational endpoints:");
    info!("  GET /v1/models");
    info!("  GET /health");
    info!("  GET /bank");
    info!("  GET /v1/pool");
    info!("  GET /proxies");
    info!("  GET /usage/overview");
    info!("  GET /usage/session/:session_id");
    info!("  POST /usage/cap");
    info!("  POST /usage/reset");
    info!(
        "Usage session keys: OpenAI=user, Anthropic=metadata.session_id|metadata.user_id, fallback=default"
    );
    info!(
        "Tor config: ports={:?}, instances={}, server=http://{}:{}",
        cfg.proxy.tor_ports, cfg.proxy.tor_instances, cfg.server.host, cfg.server.port
    );
}

fn configured_initial_tor_ports(cfg: &config::Config) -> Vec<u16> {
    let mut ports = Vec::new();
    ports.extend(cfg.provider_proxies.use_ai_ports.iter().take(2).copied());
    ports.extend(cfg.provider_proxies.sakana_ports.iter().take(1).copied());
    ports.extend(cfg.provider_proxies.faceb_ports.iter().take(1).copied());
    if ports.is_empty() {
        ports.extend(cfg.proxy.tor_ports.iter().copied());
    }
    if ports.is_empty() {
        ports.extend(
            (0..cfg.proxy.tor_instances.max(1)).map(|idx| 9050u16.saturating_add(idx as u16)),
        );
    }
    ports.sort_unstable();
    ports.dedup();
    ports
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
