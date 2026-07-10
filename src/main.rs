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
mod temp_mail;
mod usage;
mod utils;

use anyhow::Result;
use std::net::SocketAddr;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use load_monitor::LoadMonitor;

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

    let load_monitor = LoadMonitor::new();

    // Direct-egress only — no Tor. The account pool provisions use.ai accounts
    // via direct egress; Faceb warmup also runs direct with no proxies.
    let pool = account_pool::AccountPool::new(
        cfg.account_pool.size,
        Duration::from_secs(cfg.account_pool.ttl_sec),
        cfg.account_pool.refill_sec,
        cfg.account_pool.signup_delay_ms,
    )
    .await;
    pool.start().await;
    providers::faceb::start_background_warmup(Vec::new()).await;

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = api::create_routes(pool.clone(), load_monitor, cfg.clone()).layer(cors);

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
        "Egress: direct only (no Tor), server=http://{}:{}",
        cfg.server.host, cfg.server.port
    );
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
