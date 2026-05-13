//! Sync-engine: orchestrates delta polling, conflict detection, and file I/O.
//!
//! The engine is pure logic — all I/O flows through port traits injected by the
//! daemon wiring layer. Internal submodules are kept small and single-purpose.

pub mod case_collision;
pub mod conflict;
pub mod cycle;
pub mod executor;
pub mod observability;
pub mod planner;
pub mod reconcile;
pub mod retry;
pub mod scheduler;
pub mod types;

pub use cycle::run_cycle;
pub use types::{CycleSummary, EngineError};
