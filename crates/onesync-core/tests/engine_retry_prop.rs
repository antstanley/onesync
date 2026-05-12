//! Property tests for `backoff_delay` bounds.
//!
//! Asserts:
//! - Returns `None` for `attempt >= RETRY_MAX_ATTEMPTS`.
//! - Delay is monotonically non-decreasing in `attempt` when `jitter = 1.0`.
//! - Delay is always in `[0, base * 2^attempt)`.
//! - Total elapsed across all attempts (jitter = 1.0) is bounded.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use onesync_core::engine::retry::backoff_delay;
use onesync_core::limits::{RETRY_BACKOFF_BASE_MS, RETRY_MAX_ATTEMPTS};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..Default::default() })]

    /// Returns `None` for attempt >= RETRY_MAX_ATTEMPTS.
    #[test]
    fn beyond_max_returns_none(extra in 0u32..10u32) {
        let attempt = RETRY_MAX_ATTEMPTS + extra;
        prop_assert!(backoff_delay(attempt, 1.0).is_none());
    }

    /// Returns `Some` for attempt < RETRY_MAX_ATTEMPTS with any valid jitter.
    #[test]
    fn within_max_returns_some(
        attempt in 0u32..RETRY_MAX_ATTEMPTS,
        jitter in 0.0f64..=1.0f64,
    ) {
        prop_assert!(backoff_delay(attempt, jitter).is_some());
    }

    /// Delay with jitter=1.0 is monotonically non-decreasing as attempt grows.
    #[test]
    fn monotone_at_full_jitter(
        a in 0u32..RETRY_MAX_ATTEMPTS.saturating_sub(1),
    ) {
        let b = a + 1;
        if b >= RETRY_MAX_ATTEMPTS {
            return Ok(());
        }
        let da = backoff_delay(a, 1.0).unwrap().as_millis();
        let db = backoff_delay(b, 1.0).unwrap().as_millis();
        prop_assert!(db >= da, "delay({b}) = {db} < delay({a}) = {da}");
    }

    /// Delay is always in [0, base * 2^attempt] (with jitter in [0,1]).
    #[test]
    fn delay_bounded_by_cap(
        attempt in 0u32..RETRY_MAX_ATTEMPTS,
        jitter in 0.0f64..=1.0f64,
    ) {
        let delay = backoff_delay(attempt, jitter).unwrap();
        let cap_ms = RETRY_BACKOFF_BASE_MS.checked_shl(attempt).unwrap_or(u64::MAX);
        prop_assert!(
            delay.as_millis() <= u128::from(cap_ms),
            "delay={} exceeds cap={cap_ms} for attempt={attempt} jitter={jitter}",
            delay.as_millis(),
        );
    }

    /// Zero jitter always yields zero delay.
    #[test]
    fn zero_jitter_yields_zero(attempt in 0u32..RETRY_MAX_ATTEMPTS) {
        let delay = backoff_delay(attempt, 0.0).unwrap();
        prop_assert_eq!(delay.as_millis(), 0);
    }

    /// Total elapsed at full jitter across all attempts is bounded by
    /// base * (2^RETRY_MAX_ATTEMPTS - 1) (geometric series sum).
    #[test]
    fn total_elapsed_bounded(_dummy in 0u8..1u8) {
        let total: u128 = (0..RETRY_MAX_ATTEMPTS)
            .map(|a| backoff_delay(a, 1.0).unwrap().as_millis())
            .sum();
        // Upper bound: sum of geometric series base*(1 + 2 + 4 + ... + 2^(N-1)) = base*(2^N - 1).
        let max_total = u128::from(RETRY_BACKOFF_BASE_MS) * ((1u128 << RETRY_MAX_ATTEMPTS) - 1);
        prop_assert!(
            total <= max_total,
            "total={total} > max_total={max_total}",
        );
    }
}
