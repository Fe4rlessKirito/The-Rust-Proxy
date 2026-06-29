//! In-memory warm account pool with dynamic proxy rotation.

use anyhow::Result;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};
use tokio::time;
use tracing::{debug, warn};

use crate::direct::create_account;
use crate::tor_manager::TorManager;
use crate::utils::now_secs;

#[derive(Debug, Clone)]
pub struct Account {
    pub email: String,
    pub user_id: String,
    pub cookie_header: String,
    pub token: String,
    pub(crate) born: f64,
}

impl Account {
    pub fn is_fresh(&self, ttl: Duration) -> bool {
        (now_secs() - self.born) < ttl.as_secs_f64()
    }
}

pub struct AccountPool {
    inner: Arc<Mutex<VecDeque<Account>>>,
    size: usize,
    ttl: Duration,
    tor_manager: Arc<TorManager>,
    proxy_list: Arc<Mutex<Vec<String>>>,
    proxy_index: Arc<AtomicUsize>,
    pending_signups: Arc<AtomicUsize>,
    pending_proxies: Arc<Mutex<HashSet<String>>>,
    refill_interval: Duration,
    signup_delay: Duration,
    running: Arc<Mutex<bool>>,
    semaphore: Arc<Semaphore>,
}

impl AccountPool {
    pub async fn new(
        size: usize,
        ttl: Duration,
        tor_manager: Arc<TorManager>,
        refill_sec: u64,
        signup_delay_ms: u64,
    ) -> Self {
        let initial_proxies = tor_manager.get_proxies().await;
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(size))),
            size,
            ttl,
            tor_manager,
            proxy_list: Arc::new(Mutex::new(initial_proxies)),
            proxy_index: Arc::new(AtomicUsize::new(0)),
            pending_signups: Arc::new(AtomicUsize::new(0)),
            pending_proxies: Arc::new(Mutex::new(HashSet::new())),
            refill_interval: Duration::from_secs(refill_sec),
            signup_delay: Duration::from_millis(signup_delay_ms),
            running: Arc::new(Mutex::new(false)),
            semaphore: Arc::new(Semaphore::new(32)),
        }
    }

    pub async fn next_proxy(&self) -> Option<String> {
        let proxies = self.proxy_list.lock().await;
        if proxies.is_empty() {
            return None;
        }
        let idx = self.proxy_index.fetch_add(1, Ordering::Relaxed) % proxies.len();
        Some(proxies[idx].clone())
    }

    pub async fn proxies(&self) -> Vec<String> {
        self.proxy_list.lock().await.clone()
    }

    pub async fn start(&self) {
        let mut guard = self.running.lock().await;
        if *guard {
            return;
        }
        *guard = true;
        drop(guard);

        let mut rx = self.tor_manager.subscribe();
        let pool = self.clone();
        tokio::spawn(async move {
            while rx.changed().await.is_ok() {
                let new_list = rx.borrow_and_update().clone();
                let mut list = pool.proxy_list.lock().await;
                *list = new_list;
                debug!("Updated dynamic proxy list: {:?}", *list);
            }
        });

        let pool = self.clone();
        tokio::spawn(async move {
            while *pool.running.lock().await {
                let proxies = pool.proxies().await;
                let signup_targets = if proxies.is_empty() {
                    vec![None]
                } else {
                    proxies.into_iter().map(Some).collect()
                };

                let current_len = pool.inner.lock().await.len();
                let pending = pool.pending_signups.load(Ordering::Relaxed);
                let mut remaining = pool.size.saturating_sub(current_len + pending);
                let was_full = remaining == 0;

                for proxy in signup_targets {
                    if remaining == 0 {
                        break;
                    }

                    let proxy_key = proxy.clone().unwrap_or_else(|| "direct".to_string());
                    {
                        let mut pending_proxies = pool.pending_proxies.lock().await;
                        if pending_proxies.contains(&proxy_key) {
                            continue;
                        }
                        pending_proxies.insert(proxy_key.clone());
                    }

                    remaining -= 1;
                    pool.pending_signups.fetch_add(1, Ordering::Relaxed);

                    let permit = pool.semaphore.clone().acquire_owned().await.unwrap();
                    let pool = pool.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        debug!("Creating account with proxy: {:?}", proxy);
                        if let Ok(acc) = create_account(proxy.as_deref()).await {
                            let mut inner = pool.inner.lock().await;
                            if inner.len() < pool.size {
                                inner.push_back(acc);
                                debug!("Account created, pool size: {}", inner.len());
                            }
                        } else {
                            warn!("Failed to create account with proxy: {:?}", proxy);
                        }
                        pool.pending_proxies.lock().await.remove(&proxy_key);
                        pool.pending_signups.fetch_sub(1, Ordering::Relaxed);
                    });
                }

                let sleep_for = if was_full {
                    pool.refill_interval
                } else {
                    pool.signup_delay
                };
                time::sleep(sleep_for).await;
            }
        });
    }

    pub async fn stop(&self) {
        let mut guard = self.running.lock().await;
        *guard = false;
    }

    pub async fn acquire(&self) -> Result<Account> {
        {
            let mut inner = self.inner.lock().await;
            while let Some(acc) = inner.pop_front() {
                if acc.is_fresh(self.ttl) {
                    return Ok(acc);
                }
            }
        }

        let proxy = self.next_proxy().await;
        create_account(proxy.as_deref()).await
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}

impl Clone for AccountPool {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            size: self.size,
            ttl: self.ttl,
            tor_manager: self.tor_manager.clone(),
            proxy_list: self.proxy_list.clone(),
            proxy_index: self.proxy_index.clone(),
            pending_signups: self.pending_signups.clone(),
            pending_proxies: self.pending_proxies.clone(),
            refill_interval: self.refill_interval,
            signup_delay: self.signup_delay,
            running: self.running.clone(),
            semaphore: self.semaphore.clone(),
        }
    }
}
