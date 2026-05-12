//! Exponential-backoff retry helpers for transient port errors.

use crate::limits::{RETRY_BACKOFF_BASE_MS, RETRY_MAX_ATTEMPTS};

/// Outcome of a single retry computation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetryDecision {
    /// Try the operation immediately (first attempt).
    Immediate,
    /// Wait `delay_ms` milliseconds before the next attempt.
    Backoff {
        /// Milliseconds to wait before retrying.
        delay_ms: u64,
    },
    /// No more attempts; propagate the last error.
    Exhausted,
}

/// Compute the retry decision for `attempt` (0-indexed).
///
/// Uses full-jitter exponential backoff:
/// `delay = rand(0 .. BASE * 2^attempt)`, capped so it stays reasonable.
///
/// # Arguments
///
/// * `attempt` — how many attempts have already been made (0 = first try).
/// * `jitter_fraction` — a value in `[0.0, 1.0)` supplied by the caller
///   (typically from a random source) to apply jitter.
#[must_use]
pub fn retry_decision(attempt: u32, jitter_fraction: f64) -> RetryDecision {
    if attempt == 0 {
        return RetryDecision::Immediate;
    }
    if attempt >= RETRY_MAX_ATTEMPTS {
        return RetryDecision::Exhausted;
    }
    // Exponential ceiling: BASE * 2^(attempt-1), but cap at 64× base.
    let shift = attempt.saturating_sub(1).min(6);
    let cap_factor: u64 = 1u64 << shift;
    let ceiling_ms = RETRY_BACKOFF_BASE_MS.saturating_mul(cap_factor);
    // Full jitter: uniform random in [0, ceiling_ms).
    let jitter_fraction = jitter_fraction.clamp(0.0, 1.0 - f64::EPSILON);
    // LINT: cast_precision_loss — ceiling_ms is at most 64_000, well within f64 precision.
    // LINT: cast_possible_truncation and cast_sign_loss — jitter_fraction is in [0,1).
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    let delay_ms = (jitter_fraction * ceiling_ms as f64) as u64;
    RetryDecision::Backoff { delay_ms }
}

/// Return `true` if `attempt` is still within the retry budget.
#[must_use]
pub const fn should_retry(attempt: u32) -> bool {
    attempt < RETRY_MAX_ATTEMPTS
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn first_attempt_is_immediate() {
        assert_eq!(retry_decision(0, 0.5), RetryDecision::Immediate);
    }

    #[test]
    fn exhausted_at_max_attempts() {
        assert_eq!(
            retry_decision(RETRY_MAX_ATTEMPTS, 0.5),
            RetryDecision::Exhausted
        );
    }

    #[test]
    fn backoff_delay_is_within_ceiling() {
        for attempt in 1..RETRY_MAX_ATTEMPTS {
            let shift = attempt.saturating_sub(1).min(6);
            let cap_factor: u64 = 1u64 << shift;
            let ceiling = RETRY_BACKOFF_BASE_MS.saturating_mul(cap_factor);
            let RetryDecision::Backoff { delay_ms } = retry_decision(attempt, 0.9999) else {
                panic!("expected Backoff for attempt={attempt}");
            };
            assert!(delay_ms < ceiling, "delay {delay_ms} >= ceiling {ceiling}");
        }
    }

    #[test]
    fn zero_jitter_gives_zero_delay() {
        let RetryDecision::Backoff { delay_ms } = retry_decision(1, 0.0) else {
            panic!("expected Backoff");
        };
        assert_eq!(delay_ms, 0);
    }
}
