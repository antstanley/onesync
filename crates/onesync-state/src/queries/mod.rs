//! Per-table query helpers.
//!
//! Each submodule is plain Rust free functions taking `&rusqlite::Connection`.
//! The async dispatch lives in `crate::store::SqliteStore` (Task 8).

pub mod accounts;
pub mod audit;
pub mod conflicts;
pub mod file_entries;
pub mod file_ops;
pub mod pairs;
pub mod sync_runs;

/// Parse an ISO-8601 timestamp from a `SQLite` TEXT column into a typed `Timestamp`.
pub(crate) fn parse_timestamp(
    s: &str,
) -> rusqlite::Result<onesync_protocol::primitives::Timestamp> {
    let dt = chrono::DateTime::parse_from_rfc3339(s)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    Ok(onesync_protocol::primitives::Timestamp::from_datetime(
        dt.with_timezone(&chrono::Utc),
    ))
}
