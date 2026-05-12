//! `SQLite` connection pool and PRAGMAs.

use std::path::Path;
use std::path::PathBuf;

use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;

use onesync_core::limits::STATE_POOL_SIZE;
use onesync_protocol::primitives::Timestamp;

use crate::error::StateStoreError;

/// Pool of `SQLite` connections plus the database file path.
#[derive(Clone, Debug)]
pub struct ConnectionPool {
    inner: Pool<SqliteConnectionManager>,
    path: PathBuf,
}

impl ConnectionPool {
    /// Borrow a connection from the pool.
    ///
    /// # Errors
    /// Returns an error if no connection is available within the pool's timeout.
    pub fn get(&self) -> Result<PooledConnection<SqliteConnectionManager>, StateStoreError> {
        self.inner
            .get()
            .map_err(|e| StateStoreError::Sqlite(format!("pool: {e}")))
    }

    /// The on-disk path of the database.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Open (or create) the database at `path`, apply all pending migrations,
/// set the standard PRAGMAs, bootstrap the `instance_config` singleton row,
/// and return a connection pool.
///
/// `now` is used as the `updated_at` timestamp when the default config row is
/// first inserted.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` for I/O / pool failures and
/// `StateStoreError::Migration` for migration failures.
pub fn open(path: &Path, now: &Timestamp) -> Result<ConnectionPool, StateStoreError> {
    let manager = SqliteConnectionManager::file(path).with_init(|conn| {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5_000_i64)?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        Ok(())
    });

    let pool = Pool::builder()
        .max_size(u32::try_from(STATE_POOL_SIZE).unwrap_or(4))
        .build(manager)
        .map_err(|e| StateStoreError::Sqlite(format!("build pool: {e}")))?;

    // Apply migrations on a single connection before publishing the pool.
    let mut conn = pool
        .get()
        .map_err(|e| StateStoreError::Sqlite(format!("migration conn: {e}")))?;
    crate::migrations::run(&mut conn)?;

    // Bootstrap the instance_config singleton if not yet present.
    crate::queries::config::ensure_present(&conn, now)?;
    drop(conn);

    Ok(ConnectionPool {
        inner: pool,
        path: path.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queries::test_timestamp;
    use tempfile::TempDir;

    #[test]
    fn open_creates_file_and_sets_wal_mode() {
        let tmp = TempDir::new().expect("tmpdir");
        let db_path = tmp.path().join("test.sqlite");
        let pool = open(&db_path, &test_timestamp()).expect("open");
        assert!(db_path.exists(), "db file should be created");

        let conn = pool.get().expect("get conn");
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("pragma");
        assert_eq!(mode, "wal");
    }

    #[test]
    fn open_is_idempotent_across_reopens() {
        let tmp = TempDir::new().expect("tmpdir");
        let db_path = tmp.path().join("test.sqlite");

        let pool1 = open(&db_path, &test_timestamp()).expect("first open");
        drop(pool1);
        let _pool2 = open(&db_path, &test_timestamp()).expect("second open");
    }
}
