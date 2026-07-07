use anyhow::Result;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::providers::ProviderPoolStats;

use super::account::make_account;
use super::{
    FacebPoolSnapshot, FACEB_POOL_BATCH, FACEB_POOL_FILE, FACEB_POOL_MAX,
    FACEB_REFILL_INTERVAL_SEC, FACEB_WARM_TARGET,
};

struct FacebKeyPool {
    keys: Mutex<VecDeque<String>>,
    proxies: Mutex<Vec<String>>,
    proxy_index: AtomicU64,
    running: Mutex<bool>,
    refill_task: Mutex<Option<JoinHandle<()>>>,
    refill_lock: Mutex<()>,
    generated: AtomicU64,
    failed: AtomicU64,
    dead: AtomicU64,
    last_error: Mutex<Option<String>>,
}

static FACEB_POOL: once_cell::sync::Lazy<FacebKeyPool> =
    once_cell::sync::Lazy::new(FacebKeyPool::new);

impl FacebKeyPool {
    fn new() -> Self {
        let initial_keys = load_keys_with_seeds();
        let initial_len = initial_keys.len();
        if initial_len > 0 {
            let _ = save_keys(&initial_keys);
        }
        info!("Faceb key pool loaded with {} ready keys", initial_len);
        Self {
            keys: Mutex::new(initial_keys),
            proxies: Mutex::new(Vec::new()),
            proxy_index: AtomicU64::new(0),
            running: Mutex::new(false),
            refill_task: Mutex::new(None),
            refill_lock: Mutex::new(()),
            generated: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            dead: AtomicU64::new(0),
            last_error: Mutex::new(None),
        }
    }

    async fn snapshot(&self) -> FacebPoolSnapshot {
        FacebPoolSnapshot {
            pool_size: self.keys.lock().await.len(),
            gen_total: self.generated.load(Ordering::Relaxed),
            fail_total: self.failed.load(Ordering::Relaxed),
            dead_keys: self.dead.load(Ordering::Relaxed),
            last_error: self.last_error.lock().await.clone(),
        }
    }

    async fn get(&self, proxy_url: Option<&str>) -> Option<String> {
        if let Some(key) = self.keys.lock().await.pop_front() {
            return Some(key);
        }
        self.refill(proxy_url).await;
        self.keys.lock().await.pop_front()
    }

    async fn start(&'static self, proxies: Vec<String>) {
        let mut running = self.running.lock().await;
        if *running {
            return;
        }
        *running = true;
        drop(running);

        *self.proxies.lock().await = proxies;

        let handle = tokio::spawn(async move {
            while *self.running.lock().await {
                if self.keys.lock().await.len() < FACEB_WARM_TARGET {
                    let proxy_url = self.next_proxy().await;
                    self.refill(proxy_url.as_deref()).await;
                }
                tokio::time::sleep(Duration::from_secs(FACEB_REFILL_INTERVAL_SEC)).await;
            }
        });
        *self.refill_task.lock().await = Some(handle);
    }

    async fn stop(&self) {
        *self.running.lock().await = false;
        if let Some(handle) = self.refill_task.lock().await.take() {
            handle.abort();
        }
    }

    async fn set_proxies(&self, proxies: Vec<String>) {
        *self.proxies.lock().await = proxies;
    }

    async fn next_proxy(&self) -> Option<String> {
        let proxies = self.proxies.lock().await;
        if proxies.is_empty() {
            return None;
        }
        let idx = self.proxy_index.fetch_add(1, Ordering::Relaxed) as usize % proxies.len();
        Some(proxies[idx].clone())
    }

    async fn refill(&self, proxy_url: Option<&str>) {
        let _guard = self.refill_lock.lock().await;
        if self.keys.lock().await.len() >= FACEB_POOL_MAX {
            return;
        }
        let tasks = (0..FACEB_POOL_BATCH)
            .map(|_| {
                let proxy_url = proxy_url.map(ToOwned::to_owned);
                tokio::spawn(async move { make_account(proxy_url.as_deref()).await })
            })
            .collect::<Vec<_>>();
        let mut keys = Vec::new();
        for task in tasks {
            match task.await {
                Ok(Ok(Some(key))) => keys.push(key),
                Ok(Ok(None)) => {
                    self.failed.fetch_add(1, Ordering::Relaxed);
                    self.set_last_error("Faceb generation returned no key")
                        .await;
                    warn!("Faceb generation returned no key");
                }
                Ok(Err(err)) => {
                    self.failed.fetch_add(1, Ordering::Relaxed);
                    let error = format!("Faceb generation failed: {}", err);
                    self.set_last_error(error.clone()).await;
                    warn!("{}", error);
                }
                Err(err) => {
                    self.failed.fetch_add(1, Ordering::Relaxed);
                    let error = format!("Faceb generation task failed: {}", err);
                    self.set_last_error(error.clone()).await;
                    warn!("{}", error);
                }
            }
        }
        self.generated
            .fetch_add(keys.len() as u64, Ordering::Relaxed);
        let mut queue = self.keys.lock().await;
        for key in keys {
            if queue.len() >= FACEB_POOL_MAX {
                break;
            }
            queue.push_back(key);
        }
        if !queue.is_empty() {
            self.clear_last_error().await;
        }
        info!("Faceb pool size: {} ready keys", queue.len());
        let _ = save_keys(&queue);
    }

    fn mark_dead(&self) {
        self.dead.fetch_add(1, Ordering::Relaxed);
    }

    async fn set_last_error(&self, error: impl Into<String>) {
        *self.last_error.lock().await = Some(error.into());
    }

    async fn clear_last_error(&self) {
        *self.last_error.lock().await = None;
    }
}

pub(super) async fn pool_stats() -> ProviderPoolStats {
    let ready = FACEB_POOL.keys.lock().await.len();
    let last_error = FACEB_POOL.last_error.lock().await.clone();
    ProviderPoolStats {
        provider: "faceb",
        ready,
        target: Some(FACEB_POOL_MAX),
        generated: Some(FACEB_POOL.generated.load(Ordering::Relaxed)),
        failed: Some(FACEB_POOL.failed.load(Ordering::Relaxed)),
        dead: Some(FACEB_POOL.dead.load(Ordering::Relaxed)),
        cooling: None,
        degraded: ready == 0 || last_error.is_some(),
        last_error,
    }
}

pub(super) async fn pool_snapshot() -> FacebPoolSnapshot {
    FACEB_POOL.snapshot().await
}

pub(super) async fn start_background_warmup(proxies: Vec<String>) {
    FACEB_POOL.start(proxies).await;
}

pub(super) async fn set_proxies(proxies: Vec<String>) {
    FACEB_POOL.set_proxies(proxies).await;
}

pub(super) async fn stop_background_warmup() {
    FACEB_POOL.stop().await;
}

pub(super) async fn get_key(proxy_url: Option<&str>) -> Option<String> {
    FACEB_POOL.get(proxy_url).await
}

pub(super) fn mark_dead() {
    FACEB_POOL.mark_dead();
}

pub(super) async fn set_last_error(error: impl Into<String>) {
    FACEB_POOL.set_last_error(error).await;
}

pub(super) async fn clear_last_error() {
    FACEB_POOL.clear_last_error().await;
}

fn faceb_pool_path() -> PathBuf {
    let data_dir = std::env::var("LEECH_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".leech-rs"));
    data_dir.join(FACEB_POOL_FILE)
}

fn load_keys() -> VecDeque<String> {
    fs::read_to_string(faceb_pool_path())
        .ok()
        .and_then(|data| serde_json::from_str::<VecDeque<String>>(&data).ok())
        .unwrap_or_default()
}

fn load_keys_with_seeds() -> VecDeque<String> {
    let mut seen = HashSet::new();
    let mut keys = VecDeque::new();

    for key in load_keys() {
        push_key_if_valid(&mut keys, &mut seen, key);
    }

    if let Ok(key) = std::env::var("FACEB_SEED_KEY") {
        push_key_if_valid(&mut keys, &mut seen, key);
    }

    if let Ok(path) = std::env::var("FACEB_SEED_FILE") {
        match fs::read_to_string(path.trim()) {
            Ok(data) => {
                for line in data.lines() {
                    push_key_if_valid(&mut keys, &mut seen, line.to_string());
                }
            }
            Err(e) => warn!("Failed to read FACEB_SEED_FILE: {}", e),
        }
    }

    keys
}

fn push_key_if_valid(keys: &mut VecDeque<String>, seen: &mut HashSet<String>, key: String) {
    let key = key.trim().to_string();
    if key.starts_with("sk-faceb-") && seen.insert(key.clone()) {
        keys.push_back(key);
    }
}

fn save_keys(keys: &VecDeque<String>) -> Result<()> {
    let path = faceb_pool_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(keys)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}
