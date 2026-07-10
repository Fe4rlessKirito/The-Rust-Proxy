//! Provider proxy assignment tracking.
//!
//! The gateway is direct-egress only (no Tor), so there are no active SOCKS5
//! proxy assignments. The module is kept so `/proxies` and the dashboard can
//! still query provider assignments (always empty now) without special-casing.

use crate::config::ProviderProxyConfig;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use tokio::sync::Mutex;

/// Always empty now — direct egress only.
pub async fn assignments() -> HashMap<&'static str, Vec<String>> {
    let _ = PROVIDER_PROXIES.lock().await;
    HashMap::new()
}

/// Always `None` now — direct egress only. Faceb (the only
/// `ProviderRoundRobin` consumer) connects directly.
pub async fn next_proxy(_provider: &str) -> Option<String> {
    None
}

/// Configured (but inactive) provider route counts, for dashboard display.
pub fn configured_route_counts(config: &ProviderProxyConfig) -> HashMap<&'static str, usize> {
    let mut map = HashMap::new();
    map.insert("use_ai", config.use_ai_ports.len());
    map.insert("sakana", config.sakana_ports.len());
    map.insert("faceb", config.faceb_ports.len());
    map
}

/// Placeholder state retained so the type exists; never populated.
static PROVIDER_PROXIES: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));
