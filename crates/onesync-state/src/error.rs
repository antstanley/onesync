//! Internal error type, mapped to `StateError` at the port boundary.

/// Errors raised by `onesync-state` internals.
#[derive(Debug, thiserror::Error)]
pub enum StateStoreError {
    /// Underlying `SQLite` / pool failure.
    #[error("sqlite: {0}")]
    Sqlite(String),
    /// Migration failure.
    #[error("migration: {0}")]
    Migration(String),
    /// Schema mismatch detected at open time.
    #[error("schema: {0}")]
    Schema(String),
}
