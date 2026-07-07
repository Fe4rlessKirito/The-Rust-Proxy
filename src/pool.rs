//! Concurrency cap for the direct headless path.

use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

lazy_static::lazy_static! {
    static ref DIRECT_SEM: Arc<Semaphore> = Arc::new(Semaphore::new(
        std::env::var("DIRECT_MAX_CONCURRENCY")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(24)
    ));
}

pub async fn acquire_direct_permit() -> Result<OwnedSemaphorePermit, tokio::sync::AcquireError> {
    DIRECT_SEM.clone().acquire_owned().await
}
