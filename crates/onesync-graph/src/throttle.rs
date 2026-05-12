//! Per-account token-bucket throttling.
//!
//! Wraps a [`tokio::sync::Semaphore`] with a background task that refills permits
//! at [`GRAPH_RPS_PER_ACCOUNT`] per second, burst capacity 2× the rate.

use std::sync::Arc;
use tokio::sync::Semaphore;

/// Per-account token-bucket: rate `GRAPH_RPS_PER_ACCOUNT`, burst 2×.
///
/// Callers `await` [`Bucket::acquire`] before every outbound HTTP request to stay
/// well inside Microsoft Graph's documented per-account limits.
#[derive(Clone)]
pub struct Bucket {
    sem: Arc<Semaphore>,
}

impl Bucket {
    /// Create a new token bucket initialised to full burst capacity.
    ///
    /// Spawns a background Tokio task that adds one permit every
    /// `1_000 / rate_rps` milliseconds.
    #[must_use]
    pub fn new() -> Self {
        Self::with_rate(onesync_core::limits::GRAPH_RPS_PER_ACCOUNT)
    }

    /// Create with an explicit rate (useful for testing).
    #[must_use]
    pub fn with_rate(rate_rps: u32) -> Self {
        let burst = (rate_rps * 2) as usize;
        let sem = Arc::new(Semaphore::new(burst));
        let sem_clone = Arc::clone(&sem);
        let interval_ms = 1_000u64 / u64::from(rate_rps);

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_millis(interval_ms));
            ticker.tick().await; // skip the immediate first tick
            loop {
                ticker.tick().await;
                // Add a permit back up to the burst cap.
                if sem_clone.available_permits() < burst {
                    sem_clone.add_permits(1);
                }
            }
        });

        Self { sem }
    }

    /// Wait until a permit is available, then consume it.
    pub async fn acquire(&self) {
        // LINT: The semaphore is never closed; expect is appropriate here.
        #[allow(clippy::expect_used)]
        self.sem
            .acquire()
            .await
            .expect("semaphore should never be closed")
            .forget();
    }
}

impl Default for Bucket {
    fn default() -> Self {
        Self::new()
    }
}
