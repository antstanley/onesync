//! Concrete `Clock`, `IdGenerator`, and `Jitter` adapters.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod fakes;
pub mod system_clock;
pub mod system_jitter;
pub mod ulid_generator;

pub use system_clock::SystemClock;
pub use system_jitter::SystemJitter;
pub use ulid_generator::UlidGenerator;
