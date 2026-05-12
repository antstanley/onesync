//! Wall-clock `Clock` adapter.

use chrono::Utc;
use onesync_core::ports::Clock;
use onesync_protocol::primitives::Timestamp;

/// `Clock` adapter that returns the host's wall-clock UTC time.
#[derive(Default, Debug)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[allow(clippy::disallowed_methods)]
    // LINT: this is the port-impl; the disallowance is meant for engine code, not for this adapter.
    fn now(&self) -> Timestamp {
        Timestamp::from_datetime(Utc::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Utc};

    #[test]
    #[allow(clippy::disallowed_methods)]
    // LINT: test code needs the real wall clock to verify the adapter's output is recent.
    fn system_clock_returns_a_recent_timestamp() {
        let clock = SystemClock;
        let now = clock.now().into_inner();
        let real = Utc::now();
        let delta = (real - now).num_seconds().abs();
        assert!(delta < 5, "system clock drift was {delta}s");
        assert!(now.year() >= 2026, "year was {}", now.year());
    }
}
