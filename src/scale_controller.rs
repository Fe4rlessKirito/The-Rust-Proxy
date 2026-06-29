//! Decides when to add or remove Tor proxies based on load and pool health.

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tracing::{debug, info, warn};

use crate::account_pool::AccountPool;
use crate::load_monitor::LoadMonitor;
use crate::tor_manager::TorManager;

pub struct ScaleController {
    tor_manager: Arc<TorManager>,
    load_monitor: LoadMonitor,
    pool: AccountPool,
    target_pool_size: usize,
    min_proxies: usize,
    max_proxies: usize,
    scale_up_threshold: f64,
    scale_down_threshold: f64,
    cooldown: Duration,
    last_scale: tokio::sync::Mutex<Option<Instant>>,
}

impl ScaleController {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tor_manager: Arc<TorManager>,
        load_monitor: LoadMonitor,
        pool: AccountPool,
        target_pool_size: usize,
        min_proxies: usize,
        max_proxies: usize,
        scale_up_threshold: f64,
        scale_down_threshold: f64,
        cooldown: Duration,
    ) -> Self {
        Self {
            tor_manager,
            load_monitor,
            pool,
            target_pool_size,
            min_proxies,
            max_proxies,
            scale_up_threshold,
            scale_down_threshold,
            cooldown,
            last_scale: tokio::sync::Mutex::new(None),
        }
    }

    pub async fn run(&self) {
        loop {
            sleep(Duration::from_secs(5)).await;

            let current_proxies = self.tor_manager.count().await;
            let load = self.load_monitor.get_load().await;
            let pool_size = self.pool.len().await;
            let pool_ratio = if self.target_pool_size > 0 {
                pool_size as f64 / self.target_pool_size as f64
            } else {
                1.0
            };

            debug!(
                "Load: {:.2} req/s, Proxies: {}, Pool ratio: {:.2}",
                load, current_proxies, pool_ratio
            );

            let mut last_scale = self.last_scale.lock().await;
            if !last_scale.map_or(true, |t| t.elapsed() >= self.cooldown) {
                continue;
            }

            let should_scale_up = (load > self.scale_up_threshold || pool_ratio < 0.3)
                && current_proxies < self.max_proxies;

            if should_scale_up {
                info!(
                    "Scaling up: load={:.2}, pool_ratio={:.2}, proxies={}",
                    load, pool_ratio, current_proxies
                );
                match self.tor_manager.spawn_new_proxy().await {
                    Ok(url) => {
                        info!("New proxy spawned: {}", url);
                        *last_scale = Some(Instant::now());
                    }
                    Err(e) => warn!("Failed to spawn new Tor proxy: {}", e),
                }
                continue;
            }

            let should_scale_down = load < self.scale_down_threshold
                && pool_ratio > 0.8
                && current_proxies > self.min_proxies;

            if should_scale_down {
                let proxies = self.tor_manager.get_proxies().await;
                if let Some(last_url) = proxies.last() {
                    if let Some(port) = last_url.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()) {
                        if let Err(e) = self.tor_manager.remove_proxy(port).await {
                            warn!("Failed to remove proxy on port {}: {}", port, e);
                        } else {
                            info!("Removed proxy on port {}", port);
                            *last_scale = Some(Instant::now());
                        }
                    }
                }
            }

            self.load_monitor.reset().await;
        }
    }
}
