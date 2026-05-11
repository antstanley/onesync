//! Pure-logic core for onesync.
//!
//! Hosts the engine, the conflict policy, and the port traits. Has no I/O
//! dependencies. See [`docs/spec/02-architecture.md`](../../../../docs/spec/02-architecture.md).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod limits;
