//! Per-account token-bucket throttling.
//!
//! Wraps a [`tokio::sync::Semaphore`] with a background task that refills permits
//! at [`GRAPH_RPS_PER_ACCOUNT`] per second, burst capacity 2× the rate.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// Per-account token-bucket: rate `GRAPH_RPS_PER_ACCOUNT`, burst 2×.
///
/// Callers `await` [`Bucket::acquire`] before every outbound HTTP request to stay
/// well inside Microsoft Graph's documented per-account limits.
///
/// RP2-F2: [`Bucket::pause_for`] applies a hard pause to every caller of
/// [`Bucket::acquire`]. Adapters call it when Graph returns a
/// `Retry-After` (HTTP 429 / 503); subsequent acquires block until the
/// pause expires, so parallel calls on the same account back off
/// uniformly instead of compounding the throttle.
#[derive(Clone)]
pub struct Bucket {
    sem: Arc<Semaphore>,
    start: Instant,
    /// Milliseconds since `start` until which all acquires are paused.
    /// `0` means no active pause.
    pause_until_ms: Arc<AtomicU64>,
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
        // RP2-F10 partial: guard against rate=0 to avoid division-by-zero
        // when the test path passes a misconfiguration.
        let rate_rps = rate_rps.max(1);
        let burst = (rate_rps * 2) as usize;
        let sem = Arc::new(Semaphore::new(burst));
        let sem_clone = Arc::clone(&sem);
        let interval_ms = 1_000u64 / u64::from(rate_rps);

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(interval_ms));
            ticker.tick().await; // skip the immediate first tick
            loop {
                ticker.tick().await;
                // Add a permit back up to the burst cap.
                if sem_clone.available_permits() < burst {
                    sem_clone.add_permits(1);
                }
            }
        });

        Self {
            sem,
            start: Instant::now(),
            pause_until_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Pause every subsequent [`Self::acquire`] for at least `duration`.
    ///
    /// RP2-F2: adapters call this on `GraphError::Throttled` so other
    /// in-flight calls on the same account also back off, instead of
    /// continuing at full rate immediately and compounding the server's
    /// penalty.
    ///
    /// Calling `pause_for` again while a pause is active **extends** the
    /// pause to the later deadline. Earlier deadlines are dropped, never
    /// "added" — `pause_for(5)` followed by `pause_for(1)` is still
    /// 5 seconds, not 6.
    pub fn pause_for(&self, duration: Duration) {
        // LINT: duration is bounded by Graph's Retry-After header (seconds);
        // the as-u64 cast is safe for any realistic value.
        #[allow(clippy::cast_possible_truncation)]
        let target_ms = (self.start.elapsed() + duration).as_millis() as u64;
        // Use a CAS loop to keep the latest deadline only.
        loop {
            let current = self.pause_until_ms.load(Ordering::Relaxed);
            if target_ms <= current {
                return;
            }
            if self
                .pause_until_ms
                .compare_exchange(current, target_ms, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
    }

    /// Wait until a permit is available, then consume it. Also waits out
    /// any active pause set via [`Self::pause_for`].
    pub async fn acquire(&self) {
        loop {
            let until = self.pause_until_ms.load(Ordering::Relaxed);
            if until == 0 {
                break;
            }
            // LINT: elapsed() can exceed u64 ms only after 584M years; safe.
            #[allow(clippy::cast_possible_truncation)]
            let now = self.start.elapsed().as_millis() as u64;
            if now >= until {
                // Pause expired; clear it (best-effort; another thread may
                // have re-paused in the meantime, in which case the
                // compare_exchange fails and we re-check).
                let _ = self.pause_until_ms.compare_exchange(
                    until,
                    0,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
                continue;
            }
            tokio::time::sleep(Duration::from_millis(until - now)).await;
        }
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

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    /// RP2-F2: `pause_for` makes a subsequent `acquire` wait at least the
    /// requested duration.
    #[tokio::test]
    async fn acquire_waits_out_pause() {
        let bucket = Bucket::with_rate(100);
        let pause = Duration::from_millis(80);
        bucket.pause_for(pause);
        let start = Instant::now();
        bucket.acquire().await;
        let elapsed = start.elapsed();
        // Allow a small slack below the nominal deadline for tokio timer
        // coalescing; the meaningful invariant is "didn't fire instantly".
        let slack = Duration::from_millis(10);
        assert!(
            elapsed >= pause.saturating_sub(slack),
            "acquire returned too early: {elapsed:?} < {pause:?}"
        );
    }

    /// `pause_for` keeps the LATER deadline when called twice.
    #[tokio::test]
    async fn pause_for_keeps_later_deadline() {
        let bucket = Bucket::with_rate(100);
        bucket.pause_for(Duration::from_millis(50));
        // Shorter subsequent pause must NOT shrink the deadline.
        bucket.pause_for(Duration::from_millis(10));
        let start = Instant::now();
        bucket.acquire().await;
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(40),
            "shorter pause must not shrink deadline; got {elapsed:?}"
        );
    }

    /// No pause = no extra wait.
    #[tokio::test]
    async fn acquire_without_pause_returns_immediately() {
        let bucket = Bucket::with_rate(100);
        let start = Instant::now();
        bucket.acquire().await;
        assert!(
            start.elapsed() < Duration::from_millis(20),
            "no-pause acquire should be near-instant"
        );
    }

    /// RP2-F10 sibling: `with_rate(0)` no longer divides-by-zero. The
    /// `tokio::spawn` inside `with_rate` requires an active runtime, hence
    /// the `tokio::test` wrapper even though the assertion itself is
    /// synchronous.
    #[tokio::test]
    async fn with_rate_zero_does_not_panic() {
        let _bucket = Bucket::with_rate(0);
    }
}
