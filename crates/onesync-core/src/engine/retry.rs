//! Exponential backoff with full jitter, bounded by `RETRY_MAX_ATTEMPTS`.

use std::time::Duration;

use crate::limits::{RETRY_BACKOFF_BASE_MS, RETRY_MAX_ATTEMPTS};

/// Returns the backoff delay for the given attempt number (0-indexed).
///
/// `attempt = 0` → range `[0, base)` (small initial jitter).
/// `attempt = N` → range `[0, base * 2^N)`.
/// `attempt >= RETRY_MAX_ATTEMPTS` → `None` (caller stops retrying).
///
/// `jitter` is a 0..=1.0 fraction sampled by the caller; the function multiplies
/// `base * 2^attempt` by `jitter` so the same caller-supplied value yields a
/// deterministic delay (the engine's `Clock` port doesn't provide RNG; the caller
/// owns randomness).
#[must_use]
pub fn backoff_delay(attempt: u32, jitter: f64) -> Option<Duration> {
    if attempt >= RETRY_MAX_ATTEMPTS {
        return None;
    }
    // Saturating shift so large `attempt` values don't overflow.
    let cap_ms: u64 = RETRY_BACKOFF_BASE_MS
        .checked_shl(attempt)
        .unwrap_or(u64::MAX);
    // LINT: f64 precision is adequate for millisecond scheduling; no correctness dependency.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let jittered_ms = (cap_ms as f64 * jitter.clamp(0.0, 1.0)) as u64;
    Some(Duration::from_millis(jittered_ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_attempt_is_within_base() {
        let d = backoff_delay(0, 1.0).expect("present");
        assert!(u64::try_from(d.as_millis()).unwrap() <= RETRY_BACKOFF_BASE_MS);
    }

    #[test]
    fn delay_doubles_each_attempt() {
        let d0 = backoff_delay(0, 1.0).expect("0");
        let d1 = backoff_delay(1, 1.0).expect("1");
        let d2 = backoff_delay(2, 1.0).expect("2");
        // jitter = 1.0 returns the cap exactly.
        assert_eq!(
            u64::try_from(d0.as_millis()).unwrap(),
            RETRY_BACKOFF_BASE_MS
        );
        assert_eq!(
            u64::try_from(d1.as_millis()).unwrap(),
            RETRY_BACKOFF_BASE_MS * 2
        );
        assert_eq!(
            u64::try_from(d2.as_millis()).unwrap(),
            RETRY_BACKOFF_BASE_MS * 4
        );
    }

    #[test]
    fn zero_jitter_yields_zero_delay() {
        let d = backoff_delay(3, 0.0).expect("present");
        assert_eq!(d.as_millis(), 0);
    }

    #[test]
    fn beyond_max_attempts_returns_none() {
        assert!(backoff_delay(RETRY_MAX_ATTEMPTS, 1.0).is_none());
        assert!(backoff_delay(RETRY_MAX_ATTEMPTS + 5, 1.0).is_none());
    }
}
