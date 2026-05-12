//! `Clock` port: deterministic time source for the engine.

use onesync_protocol::primitives::Timestamp;

/// Source of the current wall-clock time.
pub trait Clock: Send + Sync {
    /// Returns the current UTC timestamp.
    fn now(&self) -> Timestamp;
}
