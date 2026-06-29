//! Concurrency limiting for the headless WebSocket path.

use std::sync::Arc;
use tokio::sync::{Semaphore, SemaphorePermit};

/// Global semaphore for direct WebSocket requests.
/// Its limit is controlled by `direct_max_concurrency` in config.
pub static DIRECT_SEM: once_cell::sync::Lazy<Arc<Semaphore>> = once_cell::sync::Lazy::new(|| {
    let cfg = crate::config::Config::load().unwrap_or_default();
    Arc::new(Semaphore::new(cfg.direct.direct_max_concurrency))
});

/// Acquire a permit for a direct request. The permit is released when dropped.
pub async fn acquire_direct_permit() -> Result<SemaphorePermit<'static>, tokio::sync::AcquireError> {
    DIRECT_SEM.acquire().await
}
