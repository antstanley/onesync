//! Test doubles for the `Clock`, `IdGenerator`, and `Jitter` ports.

// LINT: this module is the test-double surface for the Clock/IdGenerator ports;
//       mutex-poison expects are the standard pattern and don't warrant escape-hatch
//       handling at every call site.
#![allow(clippy::expect_used)]

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use onesync_core::ports::{Clock, IdGenerator, Jitter};
use onesync_protocol::{
    id::{Id, IdPrefix},
    primitives::Timestamp,
};
use ulid::Ulid;

/// A `Clock` whose value is set explicitly and advances only by `advance()`/`set()`.
#[derive(Debug)]
pub struct TestClock {
    inner: Mutex<DateTime<Utc>>,
}

impl TestClock {
    /// Create a `TestClock` pinned to `t`.
    #[must_use]
    pub const fn at(t: DateTime<Utc>) -> Self {
        Self {
            inner: Mutex::new(t),
        }
    }

    /// Advance the clock by `d`.
    pub fn advance(&self, d: Duration) {
        let mut guard = self.inner.lock().expect("test clock mutex poisoned");
        *guard += chrono::Duration::from_std(d).expect("duration fits chrono");
    }

    /// Jump to an absolute time `t`.
    pub fn set(&self, t: DateTime<Utc>) {
        *self.inner.lock().expect("test clock mutex poisoned") = t;
    }
}

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        let t = *self.inner.lock().expect("test clock mutex poisoned");
        Timestamp::from_datetime(t)
    }
}

/// An `IdGenerator` that produces deterministic IDs from a seed + monotonically-increasing counter.
#[derive(Debug)]
pub struct TestIdGenerator {
    counter: Mutex<u64>,
    seed: u64,
}

impl TestIdGenerator {
    /// Create a generator with the given seed. Two generators with the same seed produce identical
    /// ID sequences (so long as the same number of calls are made on each).
    #[must_use]
    pub const fn seeded(seed: u64) -> Self {
        Self {
            counter: Mutex::new(0),
            seed,
        }
    }
}

impl IdGenerator for TestIdGenerator {
    fn new_id<T: IdPrefix + 'static>(&self) -> Id<T> {
        let mut guard = self.counter.lock().expect("test id-gen mutex poisoned");
        *guard += 1;
        let n = *guard;
        drop(guard);
        // Pack seed and counter into a deterministic 128-bit value.
        let bits = (u128::from(self.seed) << 64) | u128::from(n);
        Id::from_ulid(Ulid::from(bits))
    }
}

/// A `Jitter` implementation that always returns a fixed value.
///
/// Useful for deterministic retry-delay assertions in tests.
pub struct FakeJitter(pub f64);

impl Jitter for FakeJitter {
    fn next(&self) -> f64 {
        self.0.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use onesync_protocol::id::PairTag;
    use std::time::Duration;

    #[test]
    fn test_clock_returns_the_set_time() {
        let clock = TestClock::at(chrono::Utc.with_ymd_and_hms(2026, 5, 11, 10, 0, 0).unwrap());
        let t1 = clock.now().into_inner();
        let t2 = clock.now().into_inner();
        assert_eq!(t1, t2);
    }

    #[test]
    fn test_clock_advance_moves_forward() {
        let clock = TestClock::at(chrono::Utc.with_ymd_and_hms(2026, 5, 11, 10, 0, 0).unwrap());
        let before = clock.now().into_inner();
        clock.advance(Duration::from_mins(1));
        let after = clock.now().into_inner();
        assert_eq!((after - before).num_seconds(), 60);
    }

    #[test]
    fn test_id_generator_is_deterministic() {
        let g = TestIdGenerator::seeded(42);
        let a: Id<PairTag> = g.new_id();
        let b: Id<PairTag> = g.new_id();
        assert_ne!(a, b);

        let g2 = TestIdGenerator::seeded(42);
        let c: Id<PairTag> = g2.new_id();
        assert_eq!(a, c, "same seed must produce same first ID");
    }
}
