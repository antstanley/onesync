//! Concrete `Clock` and `IdGenerator` adapters.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod system_clock;
pub mod ulid_generator;

pub use system_clock::SystemClock;
pub use ulid_generator::UlidGenerator;
