//! `SQLite` connection pool and PRAGMAs.

use std::path::Path;

/// Placeholder for the connection pool type; Task 3 implements it.
#[derive(Debug)]
pub struct ConnectionPool;

/// Placeholder for the open-and-migrate entry point; Task 3 implements it.
///
/// # Errors
/// Returns an error when the database cannot be opened or migrated.
#[allow(clippy::unimplemented)]
// LINT: filled in by M2 Task 3 (Connection pool + PRAGMAs).
pub fn open(_path: &Path) -> Result<ConnectionPool, crate::error::StateStoreError> {
    unimplemented!("M2 Task 3 implements this")
}
