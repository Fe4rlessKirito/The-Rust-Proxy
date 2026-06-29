//! Tracks request load for scaling decisions.

use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct LoadMonitor {
    request_count: Arc<Mutex<u64>>,
    window_start: Arc<Mutex<Instant>>,
}

impl LoadMonitor {
    pub fn new() -> Self {
        Self {
            request_count: Arc::new(Mutex::new(0)),
            window_start: Arc::new(Mutex::new(Instant::now())),
        }
    }

    pub async fn record_request(&self) {
        let mut count = self.request_count.lock().await;
        *count += 1;
    }

    pub async fn get_load(&self) -> f64 {
        let count = *self.request_count.lock().await;
        let start = *self.window_start.lock().await;
        let elapsed = start.elapsed().as_secs_f64().max(1.0);
        count as f64 / elapsed
    }

    pub async fn snapshot(&self) -> (u64, f64) {
        let count = *self.request_count.lock().await;
        (count, self.get_load().await)
    }

    pub async fn reset(&self) {
        let mut count = self.request_count.lock().await;
        *count = 0;
        let mut start = self.window_start.lock().await;
        *start = Instant::now();
    }
}
