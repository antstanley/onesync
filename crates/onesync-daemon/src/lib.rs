//! `onesync-daemon` library target.
//!
//! Exposes the daemon's internal modules so that integration tests can
//! construct in-process daemon instances with fake adapters.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod audit_sink;
pub mod ipc;
pub mod lock;
pub mod logging;
pub mod methods;
pub mod shutdown;
pub mod startup;
pub mod wiring;
