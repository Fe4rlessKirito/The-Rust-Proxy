//! Configuration management.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub direct: DirectConfig,
    pub account_pool: AccountPoolConfig,
    pub proxy: ProxyConfig,
    pub provider_proxies: ProviderProxyConfig,
    pub models: ModelsConfig,
    pub thinking: ThinkingConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DirectConfig {
    pub auth_base: String,
    pub ws_agent_base: String,
    pub model_prefix: String,
    pub ws_open_timeout_sec: u64,
    pub ws_idle_timeout_sec: u64,
    pub direct_ws_retries: u32,
    pub direct_max_concurrency: usize,
    /// When true, the proxy auto-sends a "Continue." follow-up turn on the
    /// same WebSocket if the assistant's turn looks like a premature intent
    /// announcement (e.g. "I'll fix the UI, then build a patch.") with no
    /// actual work artifact. See `looks_like_premature_intent` in direct.rs.
    pub auto_continue: bool,
    /// Maximum number of auto-continue follow-ups per request. Caps runaway
    /// loops once the heuristic stops matching.
    pub auto_continue_max: u32,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AccountPoolConfig {
    pub size: usize,
    pub ttl_sec: u64,
    pub refill_sec: u64,
    pub signup_delay_ms: u64,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ProxyConfig {
    pub socks5_url: Option<String>, // kept for backward compatibility
    pub tor_ports: Vec<u16>,        // list of ports to run Tor on
    pub tor_instances: usize,       // number of Tor instances to spawn (if ports not specified)
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ProviderProxyConfig {
    pub use_ai_ports: Vec<u16>,
    pub sakana_ports: Vec<u16>,
    pub faceb_ports: Vec<u16>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ModelsConfig {
    pub default: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ThinkingConfig {
    pub levels: HashMap<String, usize>,
    pub expose_tool_thinking: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LoggingConfig {
    pub level: String,
    pub debug_protocol: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            server: ServerConfig {
                host: "127.0.0.1".into(),
                port: 8000,
            },
            direct: DirectConfig {
                auth_base: "https://api.use.ai/v1/auth".into(),
                ws_agent_base: "wss://agents.use.ai/agents/budget-agent".into(),
                model_prefix: "gateway-".into(),
                ws_open_timeout_sec: 30,
                ws_idle_timeout_sec: 90,
                direct_ws_retries: 2,
                direct_max_concurrency: 24,
                auto_continue: true,
                auto_continue_max: 3,
            },
            account_pool: AccountPoolConfig {
                size: 100,
                ttl_sec: 1800,
                refill_sec: 5,
                signup_delay_ms: 1000,
            },
            proxy: ProxyConfig {
                socks5_url: Some("socks5h://127.0.0.1:9050".into()),
                tor_ports: vec![9050, 9051, 9052],
                tor_instances: 3,
            },
            provider_proxies: ProviderProxyConfig {
                use_ai_ports: (9050..=9060).collect(),
                sakana_ports: (9061..=9070).collect(),
                faceb_ports: (9071..=9080).collect(),
            },
            models: ModelsConfig {
                default: "gpt-5-4".into(),
            },
            thinking: ThinkingConfig {
                expose_tool_thinking: true,
                levels: [
                    ("low".into(), 1024),
                    ("medium".into(), 5000),
                    ("high".into(), 16000),
                    ("max".into(), 32000),
                ]
                .into_iter()
                .collect(),
            },
            logging: LoggingConfig {
                level: "info".into(),
                debug_protocol: false,
            },
        }
    }
}

impl Config {
    pub fn load() -> Result<Self, config::ConfigError> {
        let builder = config::Config::builder()
            .add_source(config::Config::try_from(&Config::default())?)
            .add_source(config::File::with_name("config").required(false))
            .add_source(config::Environment::with_prefix("LEECH").separator("__"))
            .build()?;
        builder.try_deserialize()
    }
}
