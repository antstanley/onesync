//! `instance_config` singleton query helpers.
//!
//! The `instance_config` table has at most one row (`id = 1`).

use rusqlite::{Connection, OptionalExtension, params};

use onesync_protocol::{config::InstanceConfig, enums::LogLevel, primitives::Timestamp};

use crate::error::StateStoreError;
use crate::queries::parse_timestamp;

/// Fetch the singleton config row.
///
/// Returns `Ok(None)` if the row does not exist yet.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or the row can't be decoded.
pub fn get(conn: &Connection) -> Result<Option<InstanceConfig>, StateStoreError> {
    conn.query_row(
        "SELECT log_level, notify, allow_metered, min_free_gib, updated_at \
         FROM instance_config WHERE id = 1",
        [],
        row_to_config,
    )
    .optional()
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Insert or update the singleton config row.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails.
pub fn upsert(conn: &Connection, cfg: &InstanceConfig) -> Result<(), StateStoreError> {
    conn.execute(
        "INSERT INTO instance_config (id, log_level, notify, allow_metered, min_free_gib, updated_at) \
         VALUES (1, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
            log_level = excluded.log_level, \
            notify = excluded.notify, \
            allow_metered = excluded.allow_metered, \
            min_free_gib = excluded.min_free_gib, \
            updated_at = excluded.updated_at",
        params![
            log_level_to_str(cfg.log_level),
            i32::from(cfg.notify),
            i32::from(cfg.allow_metered),
            i64::from(cfg.min_free_gib),
            cfg.updated_at.into_inner().to_rfc3339(),
        ],
    )
    .map(|_| ())
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Insert the default config row only if no row is present yet.
///
/// Defaults: `log_level = info`, `notify = true`, `allow_metered = false`,
/// `min_free_gib = 2`, `updated_at = now`.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails.
pub fn ensure_present(conn: &Connection, now: &Timestamp) -> Result<(), StateStoreError> {
    conn.execute(
        "INSERT OR IGNORE INTO instance_config \
            (id, log_level, notify, allow_metered, min_free_gib, updated_at) \
         VALUES (1, 'info', 1, 0, 2, ?)",
        params![now.into_inner().to_rfc3339()],
    )
    .map(|_| ())
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

fn row_to_config(row: &rusqlite::Row<'_>) -> rusqlite::Result<InstanceConfig> {
    let log_level_str: String = row.get(0)?;
    let notify_int: i32 = row.get(1)?;
    let allow_metered_int: i32 = row.get(2)?;
    let min_free_gib_i64: i64 = row.get(3)?;
    let updated_at_str: String = row.get(4)?;

    Ok(InstanceConfig {
        log_level: log_level_from_str(&log_level_str)?,
        notify: notify_int != 0,
        allow_metered: allow_metered_int != 0,
        min_free_gib: u32::try_from(min_free_gib_i64).unwrap_or(0),
        updated_at: parse_timestamp(&updated_at_str)?,
    })
}

const fn log_level_to_str(l: LogLevel) -> &'static str {
    match l {
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
        LogLevel::Trace => "trace",
    }
}

fn log_level_from_str(s: &str) -> rusqlite::Result<LogLevel> {
    match s {
        "info" => Ok(LogLevel::Info),
        "debug" => Ok(LogLevel::Debug),
        "trace" => Ok(LogLevel::Trace),
        other => Err(rusqlite::Error::InvalidColumnType(
            0,
            format!("unknown log level: {other}"),
            rusqlite::types::Type::Text,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open;
    use crate::queries::test_timestamp;
    use tempfile::TempDir;

    fn fresh_db() -> (TempDir, crate::connection::ConnectionPool) {
        let tmp = TempDir::new().expect("tmpdir");
        let pool = open(&tmp.path().join("t.sqlite"), &test_timestamp()).expect("open");
        (tmp, pool)
    }

    #[test]
    fn ensure_present_creates_default_row_when_missing() {
        let (_tmp, pool) = fresh_db();
        // `open` already calls `ensure_present`, so row must exist.
        let conn = pool.get().expect("conn");
        let cfg = get(&conn).expect("get").expect("row present");
        assert_eq!(cfg.log_level, LogLevel::Info);
        assert!(cfg.notify);
        assert!(!cfg.allow_metered);
        assert_eq!(cfg.min_free_gib, 2);
        assert_eq!(cfg.updated_at, test_timestamp());
    }

    #[test]
    fn ensure_present_is_a_no_op_on_existing_row() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");

        // Customise the row.
        let first = get(&conn).expect("get first").expect("present");
        let custom = InstanceConfig {
            log_level: LogLevel::Debug,
            notify: false,
            allow_metered: true,
            min_free_gib: 10,
            updated_at: test_timestamp(),
        };
        upsert(&conn, &custom).expect("upsert");

        // Call ensure_present again — must not overwrite.
        ensure_present(&conn, &test_timestamp()).expect("second ensure_present");

        let after = get(&conn).expect("get after").expect("present");
        // Row should still reflect our custom upsert, not the defaults.
        assert_eq!(after.log_level, LogLevel::Debug);
        assert!(!after.notify);
        assert!(after.allow_metered);
        assert_eq!(after.min_free_gib, 10);

        // Also verify the first call created a row at all.
        assert_eq!(first.log_level, LogLevel::Info);
    }

    #[test]
    fn upsert_updates_the_singleton_row() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");

        let updated = InstanceConfig {
            log_level: LogLevel::Debug,
            notify: false,
            allow_metered: false,
            min_free_gib: 5,
            updated_at: test_timestamp(),
        };
        upsert(&conn, &updated).expect("upsert");

        let back = get(&conn).expect("get").expect("present");
        assert_eq!(back.log_level, LogLevel::Debug);
        assert!(!back.notify);
        assert_eq!(back.min_free_gib, 5);
    }

    #[test]
    fn get_returns_none_on_fresh_db_before_ensure_present() {
        // Bypass `open` (which bootstraps) — apply migrations directly.
        let mut raw = rusqlite::Connection::open_in_memory().expect("in-memory db");
        crate::migrations::run(&mut raw).expect("migrations");
        let result = get(&raw).expect("get");
        assert!(
            result.is_none(),
            "no row should exist before ensure_present"
        );
    }
}
