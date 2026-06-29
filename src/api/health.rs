//! Health, bank, and config endpoints.

use axum::{
    extract::State,
    response::Json,
    routing::{get, post},
    Extension,
    Router,
};
use serde_json::{json, Value};

use crate::account_pool::AccountPool;
use crate::config::Config;

pub fn routes() -> Router<AccountPool> {
    Router::new()
        .route("/health", get(health_handler))
        .route("/bank", get(bank_handler))
        .route("/config", get(config_get_handler))
        .route("/config", post(config_post_handler))
}

async fn health_handler(State(pool): State<AccountPool>) -> Json<serde_json::Value> {
    let fresh = pool.len().await;
    Json(json!({
        "status": "ok",
        "fresh_accounts": fresh,
        "send_success_rate": 1.0,
        "reasons": ["all systems nominal"]
    }))
}

async fn bank_handler(
    State(pool): State<AccountPool>,
    Extension(cfg): Extension<Config>,
) -> Json<serde_json::Value> {
    let fresh = pool.len().await;
    Json(json!({
        "mode": "headless-ws",
        "warm_accounts": fresh,
        "pool_target": cfg.account_pool.size,
        "status": "ok"
    }))
}

async fn config_get_handler(Extension(cfg): Extension<Config>) -> Json<serde_json::Value> {
    Json(json!({
        "server_host": cfg.server.host,
        "server_port": cfg.server.port,
        "pool_size": cfg.account_pool.size,
        "signup_delay_ms": cfg.account_pool.signup_delay_ms,
        "account_ttl_sec": cfg.account_pool.ttl_sec,
        "proxy_tor": !cfg.proxy.tor_ports.is_empty(),
        "tor_socks": cfg.proxy.socks5_url.unwrap_or_default(),
        "tor_ports": cfg.proxy.tor_ports,
        "tor_instances": cfg.proxy.tor_instances,
    }))
}

async fn config_post_handler(
    State(_pool): State<AccountPool>,
    Extension(current): Extension<Config>,
    Json(payload): Json<Value>,
) -> Json<serde_json::Value> {
    let mut cfg = Config::load().unwrap_or(current);

    if let Some(server) = payload.get("server") {
        if let Some(host) = server.get("host").and_then(|v| v.as_str()) {
            cfg.server.host = host.to_string();
        }
        if let Some(port) = server
            .get("port")
            .and_then(|v| v.as_u64())
            .and_then(|v| u16::try_from(v).ok())
        {
            cfg.server.port = port;
        }
    }

    if let Some(direct) = payload.get("direct") {
        if let Some(auth_base) = direct.get("auth_base").and_then(|v| v.as_str()) {
            cfg.direct.auth_base = auth_base.to_string();
        }
        if let Some(ws_agent_base) = direct.get("ws_agent_base").and_then(|v| v.as_str()) {
            cfg.direct.ws_agent_base = ws_agent_base.to_string();
        }
        if let Some(model_prefix) = direct.get("model_prefix").and_then(|v| v.as_str()) {
            cfg.direct.model_prefix = model_prefix.to_string();
        }
        if let Some(value) = direct.get("ws_open_timeout_sec").and_then(|v| v.as_u64()) {
            cfg.direct.ws_open_timeout_sec = value;
        }
        if let Some(value) = direct.get("ws_idle_timeout_sec").and_then(|v| v.as_u64()) {
            cfg.direct.ws_idle_timeout_sec = value;
        }
        if let Some(value) = direct
            .get("direct_ws_retries")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
        {
            cfg.direct.direct_ws_retries = value;
        }
        if let Some(value) = direct
            .get("direct_max_concurrency")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
        {
            cfg.direct.direct_max_concurrency = value;
        }
    }

    if let Some(account_pool) = payload.get("account_pool") {
        if let Some(value) = account_pool
            .get("size")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
        {
            cfg.account_pool.size = value;
        }
        if let Some(value) = account_pool.get("ttl_sec").and_then(|v| v.as_u64()) {
            cfg.account_pool.ttl_sec = value;
        }
        if let Some(value) = account_pool.get("refill_sec").and_then(|v| v.as_u64()) {
            cfg.account_pool.refill_sec = value;
        }
        if let Some(value) = account_pool.get("signup_delay_ms").and_then(|v| v.as_u64()) {
            cfg.account_pool.signup_delay_ms = value;
        }
    }

    if let Some(proxy) = payload.get("proxy") {
        if proxy.get("socks5_url").is_some() {
            cfg.proxy.socks5_url = proxy
                .get("socks5_url")
                .and_then(|v| v.as_str())
                .map(ToOwned::to_owned);
        }
        if let Some(ports) = proxy.get("tor_ports").and_then(|v| v.as_array()) {
            cfg.proxy.tor_ports = ports
                .iter()
                .filter_map(|v| v.as_u64().and_then(|n| u16::try_from(n).ok()))
                .collect();
        }
        if let Some(value) = proxy
            .get("tor_instances")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
        {
            cfg.proxy.tor_instances = value;
        }
    }

    if let Some(models) = payload.get("models") {
        if let Some(default) = models.get("default").and_then(|v| v.as_str()) {
            cfg.models.default = default.to_string();
        }
    }

    if let Some(thinking) = payload.get("thinking") {
        if let Some(levels) = thinking.get("levels").and_then(|v| v.as_object()) {
            for (key, value) in levels {
                if let Some(tokens) = value.as_u64().and_then(|v| usize::try_from(v).ok()) {
                    cfg.thinking.levels.insert(key.clone(), tokens);
                }
            }
        }
    }

    match cfg.save() {
        Ok(()) => Json(json!({
            "status": "ok",
            "restart_required_for": ["server", "account_pool", "proxy"],
            "config": cfg,
        })),
        Err(e) => Json(json!({
            "error": format!("Failed to save config: {}", e)
        })),
    }
}
