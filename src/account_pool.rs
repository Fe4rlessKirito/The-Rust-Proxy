//! In-memory warm account pool. Direct-egress only (no Tor).

use anyhow::Result;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{debug, warn};

use crate::direct::create_account;
use crate::utils::now_secs;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Account {
    pub email: String,
    pub user_id: String,
    pub cookie_header: String,
    pub token: String,
    /// use.ai's agent gateway now requires an `app_token` query param on the
    /// WS URL (in addition to the JWT `token`). It is issued by get-session
    /// alongside session_data and must be refreshed alongside the session.
    pub app_token: Option<String>,
    /// The proxy this account was created through. Always `None` now that the
    /// gateway is direct-egress only; kept for API compatibility with
    /// `direct.rs` (which treats `None` as a direct connection).
    pub proxy_url: Option<String>,
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
    pending_signups: Arc<AtomicUsize>,
    refill_interval: Duration,
    signup_delay: Duration,
    running: Arc<Mutex<bool>>,
    semaphore: Arc<Semaphore>,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl AccountPool {
    pub async fn new(size: usize, ttl: Duration, refill_sec: u64, signup_delay_ms: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(size))),
            size,
            ttl,
            pending_signups: Arc::new(AtomicUsize::new(0)),
            refill_interval: Duration::from_secs(refill_sec),
            signup_delay: Duration::from_millis(signup_delay_ms),
            running: Arc::new(Mutex::new(false)),
            semaphore: Arc::new(Semaphore::new(32)),
            tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Always `None` now — direct egress only. Kept for callers that still
    /// ask for a per-request proxy (treated as "direct").
    pub async fn next_proxy(&self) -> Option<String> {
        None
    }

    pub async fn proxies(&self) -> Vec<String> {
        Vec::new()
    }

    /// No-op now that there are no proxies to set. Kept for API compatibility.
    pub async fn set_proxies(&self, _proxies: Vec<String>) {}

    pub async fn start(&self) {
        let mut guard = self.running.lock().await;
        if *guard {
            return;
        }
        *guard = true;
        drop(guard);

        let pool = self.clone();
        let refill_handle = tokio::spawn(async move {
            while *pool.running.lock().await {
                let current_len = pool.inner.lock().await.len();
                let pending = pool.pending_signups.load(Ordering::Relaxed);
                let remaining = pool.size.saturating_sub(current_len + pending);
                let was_full = remaining == 0;

                // One direct signup per refill tick. use.ai rate-limits per IP
                // and there's a single direct egress IP, so pace signups rather
                // than firing many concurrently.
                if remaining > 0 {
                    pool.pending_signups.fetch_add(1, Ordering::Relaxed);
                    let permit = pool.semaphore.clone().acquire_owned().await.unwrap();
                    let pool = pool.clone();
                    let signup_handle = tokio::spawn(async move {
                        let _permit = permit;
                        debug!("Creating account (direct egress)");
                        let result = create_account(None).await;
                        pool.pending_signups.fetch_sub(1, Ordering::Relaxed);
                        if let Ok(acc) = result {
                            let mut inner = pool.inner.lock().await;
                            if inner.len() < pool.size {
                                inner.push_back(acc);
                                debug!("Account created, pool size: {}", inner.len());
                            }
                        } else {
                            warn!("Failed to create account (direct egress)");
                        }
                    });
                    pool.tasks.lock().await.push(signup_handle);
                }

                let sleep_for = if was_full {
                    pool.refill_interval
                } else {
                    pool.signup_delay
                };
                time::sleep(sleep_for).await;
            }
        });
        self.tasks.lock().await.push(refill_handle);
    }

    pub async fn stop(&self) {
        let mut guard = self.running.lock().await;
        *guard = false;
        drop(guard);

        let mut tasks = self.tasks.lock().await;
        for handle in tasks.drain(..) {
            handle.abort();
        }
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

        // No fresh warm account — provision one on demand via direct egress.
        create_account(None).await
    }

    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    pub fn target_size(&self) -> usize {
        self.size
    }
}

impl Clone for AccountPool {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            size: self.size,
            ttl: self.ttl,
            pending_signups: self.pending_signups.clone(),
            refill_interval: self.refill_interval,
            signup_delay: self.signup_delay,
            running: self.running.clone(),
            semaphore: self.semaphore.clone(),
            tasks: self.tasks.clone(),
        }
    }
}
