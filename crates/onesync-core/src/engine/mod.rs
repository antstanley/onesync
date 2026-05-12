//! Pure-logic sync engine.
//!
//! Owns the per-pair cycle, reconciliation, conflict policy, retry/backoff,
//! and op planning. Has no I/O — composes the port traits.
//!
//! See [`docs/spec/03-sync-engine.md`](../../../../docs/spec/03-sync-engine.md).

pub mod conflict;
pub mod cycle;
pub mod executor;
pub mod observability;
pub mod planner;
pub mod reconcile;
pub mod retry;
pub mod scheduler;
pub mod types;
