//! `file_ops` query helpers.

use rusqlite::{Connection, params};

use onesync_protocol::{
    enums::{FileOpKind, FileOpStatus},
    errors::ErrorEnvelope,
    file_op::FileOp,
    id::{FileOpId, PairId},
    primitives::Timestamp,
};

use crate::error::StateStoreError;
use crate::queries::parse_timestamp;

/// Insert a new file-op row.
///
/// Operations are append-only — use [`update_status`] to mutate an existing row.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or values can't be encoded.
pub fn insert(conn: &Connection, op: &FileOp) -> Result<(), StateStoreError> {
    let last_error_json = encode_last_error(op.last_error.as_ref())?;
    let metadata_json = encode_metadata(&op.metadata)?;

    conn.execute(
        "INSERT INTO file_ops \
            (id, run_id, pair_id, relative_path, kind, status, attempts, \
             last_error_json, metadata_json, enqueued_at, started_at, finished_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            op.id.to_string(),
            op.run_id.to_string(),
            op.pair_id.to_string(),
            op.relative_path.as_str(),
            op_kind_to_str(op.kind),
            op_status_to_str(op.status),
            i64::from(op.attempts),
            last_error_json,
            metadata_json,
            op.enqueued_at.into_inner().to_rfc3339(),
            op.started_at.map(|ts| ts.into_inner().to_rfc3339()),
            op.finished_at.map(|ts| ts.into_inner().to_rfc3339()),
        ],
    )
    .map(|_| ())
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Update only the `status`, `started_at`, and `finished_at` columns of an op.
///
/// - Transitioning to `in_progress` fills `started_at` if it is currently `NULL`.
/// - Transitioning to `success` or `failed` fills `finished_at`.
/// - Other transitions leave both timestamps unchanged.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails.
pub fn update_status(
    conn: &Connection,
    id: &FileOpId,
    status: FileOpStatus,
    now: &Timestamp,
) -> Result<(), StateStoreError> {
    let now_str = now.into_inner().to_rfc3339();

    let (started_at_param, finished_at_param): (Option<&str>, Option<&str>) = match status {
        FileOpStatus::InProgress => (Some(&now_str), None),
        FileOpStatus::Success | FileOpStatus::Failed => (None, Some(&now_str)),
        FileOpStatus::Enqueued | FileOpStatus::Backoff => (None, None),
    };

    conn.execute(
        "UPDATE file_ops \
         SET status = ?, \
             started_at = COALESCE(started_at, ?), \
             finished_at = ? \
         WHERE id = ?",
        params![
            op_status_to_str(status),
            started_at_param,
            finished_at_param,
            id.to_string(),
        ],
    )
    .map(|_| ())
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Fetch all ops in `enqueued`, `in_progress`, or `backoff` state for `pair`, ordered by
/// `enqueued_at`.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or rows can't be decoded.
pub fn in_flight(conn: &Connection, pair: &PairId) -> Result<Vec<FileOp>, StateStoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, run_id, pair_id, relative_path, kind, status, attempts, \
                    last_error_json, metadata_json, enqueued_at, started_at, finished_at \
             FROM file_ops \
             WHERE pair_id = ? AND status IN ('enqueued','in_progress','backoff') \
             ORDER BY enqueued_at",
        )
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    let rows = stmt
        .query_map(params![pair.to_string()], row_to_op)
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    rows.map(|r| r.map_err(|e| StateStoreError::Sqlite(e.to_string())))
        .collect()
}

fn row_to_op(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileOp> {
    let id_str: String = row.get(0)?;
    let run_id_str: String = row.get(1)?;
    let pair_id_str: String = row.get(2)?;
    let rel_path_str: String = row.get(3)?;
    let kind_str: String = row.get(4)?;
    let status_str: String = row.get(5)?;
    let attempts_i64: i64 = row.get(6)?;
    let last_error_json: Option<String> = row.get(7)?;
    let metadata_json: Option<String> = row.get(8)?;
    let enqueued_at_str: String = row.get(9)?;
    let started_at_opt: Option<String> = row.get(10)?;
    let finished_at_opt: Option<String> = row.get(11)?;

    Ok(FileOp {
        id: id_str
            .parse()
            .map_err(|e: onesync_protocol::id::IdParseError| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            })?,
        run_id: run_id_str
            .parse()
            .map_err(|e: onesync_protocol::id::IdParseError| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            })?,
        pair_id: pair_id_str
            .parse()
            .map_err(|e: onesync_protocol::id::IdParseError| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            })?,
        relative_path: rel_path_str.parse().map_err(
            |e: onesync_protocol::path::PathParseError| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            },
        )?,
        kind: op_kind_from_str(&kind_str)?,
        status: op_status_from_str(&status_str)?,
        attempts: u32::try_from(attempts_i64).unwrap_or(0),
        last_error: decode_last_error(last_error_json.as_deref())?,
        metadata: decode_metadata(metadata_json.as_deref())?,
        enqueued_at: parse_timestamp(&enqueued_at_str)?,
        started_at: started_at_opt.map(|s| parse_timestamp(&s)).transpose()?,
        finished_at: finished_at_opt.map(|s| parse_timestamp(&s)).transpose()?,
    })
}

fn encode_last_error(err: Option<&ErrorEnvelope>) -> Result<Option<String>, StateStoreError> {
    err.map(|e| {
        serde_json::to_string(e)
            .map_err(|enc| StateStoreError::Sqlite(format!("encode last_error: {enc}")))
    })
    .transpose()
}

fn decode_last_error(json: Option<&str>) -> rusqlite::Result<Option<ErrorEnvelope>> {
    json.map(|s| {
        serde_json::from_str(s).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    })
    .transpose()
}

fn encode_metadata(
    meta: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<String>, StateStoreError> {
    if meta.is_empty() {
        return Ok(None);
    }
    serde_json::to_string(meta)
        .map(Some)
        .map_err(|e| StateStoreError::Sqlite(format!("encode metadata: {e}")))
}

fn decode_metadata(
    json: Option<&str>,
) -> rusqlite::Result<serde_json::Map<String, serde_json::Value>> {
    json.map(|s| {
        serde_json::from_str(s).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    })
    .transpose()
    .map(Option::unwrap_or_default)
}

const fn op_kind_to_str(k: FileOpKind) -> &'static str {
    match k {
        FileOpKind::Upload => "upload",
        FileOpKind::Download => "download",
        FileOpKind::LocalDelete => "local_delete",
        FileOpKind::RemoteDelete => "remote_delete",
        FileOpKind::LocalMkdir => "local_mkdir",
        FileOpKind::RemoteMkdir => "remote_mkdir",
        FileOpKind::LocalRename => "local_rename",
        FileOpKind::RemoteRename => "remote_rename",
    }
}

fn op_kind_from_str(s: &str) -> rusqlite::Result<FileOpKind> {
    match s {
        "upload" => Ok(FileOpKind::Upload),
        "download" => Ok(FileOpKind::Download),
        "local_delete" => Ok(FileOpKind::LocalDelete),
        "remote_delete" => Ok(FileOpKind::RemoteDelete),
        "local_mkdir" => Ok(FileOpKind::LocalMkdir),
        "remote_mkdir" => Ok(FileOpKind::RemoteMkdir),
        "local_rename" => Ok(FileOpKind::LocalRename),
        "remote_rename" => Ok(FileOpKind::RemoteRename),
        other => Err(rusqlite::Error::InvalidColumnType(
            4,
            format!("unknown file op kind: {other}"),
            rusqlite::types::Type::Text,
        )),
    }
}

const fn op_status_to_str(s: FileOpStatus) -> &'static str {
    match s {
        FileOpStatus::Enqueued => "enqueued",
        FileOpStatus::InProgress => "in_progress",
        FileOpStatus::Backoff => "backoff",
        FileOpStatus::Success => "success",
        FileOpStatus::Failed => "failed",
    }
}

fn op_status_from_str(s: &str) -> rusqlite::Result<FileOpStatus> {
    match s {
        "enqueued" => Ok(FileOpStatus::Enqueued),
        "in_progress" => Ok(FileOpStatus::InProgress),
        "backoff" => Ok(FileOpStatus::Backoff),
        "success" => Ok(FileOpStatus::Success),
        "failed" => Ok(FileOpStatus::Failed),
        other => Err(rusqlite::Error::InvalidColumnType(
            5,
            format!("unknown file op status: {other}"),
            rusqlite::types::Type::Text,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open;
    use crate::queries::{accounts, pairs};
    use chrono::{TimeZone, Utc};
    use onesync_core::ports::Clock;
    use onesync_protocol::{
        account::Account,
        enums::{AccountKind, PairStatus},
        id::{AccountTag, Id, PairTag, SyncRunTag},
        pair::Pair,
        primitives::{DriveId, DriveItemId, KeychainRef, Timestamp},
    };
    use onesync_time::fakes::TestClock;
    use tempfile::TempDir;
    use ulid::Ulid;

    fn fresh_db() -> (TempDir, crate::connection::ConnectionPool) {
        let tmp = TempDir::new().expect("tmpdir");
        let pool = open(
            &tmp.path().join("t.sqlite"),
            &crate::queries::test_timestamp(),
        )
        .expect("open");
        (tmp, pool)
    }

    fn base_ts() -> Timestamp {
        Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap())
    }

    fn sample_account() -> Account {
        Account {
            id: Id::<AccountTag>::from_ulid(Ulid::from(1u128 << 64)),
            kind: AccountKind::Personal,
            upn: "alice@example.com".into(),
            tenant_id: "9188040d-6c67-4c5b-b112-36a304b66dad".into(),
            drive_id: DriveId::new("drv-1"),
            display_name: "Alice".into(),
            keychain_ref: KeychainRef::new("kc-1"),
            scopes: vec!["Files.ReadWrite".into()],
            created_at: base_ts(),
            updated_at: base_ts(),
        }
    }

    fn sample_pair(account: &Account) -> Pair {
        Pair {
            id: Id::<PairTag>::from_ulid(Ulid::from(2u128 << 64)),
            account_id: account.id,
            local_path: "/Users/alice/OneDrive".parse().expect("abs path"),
            remote_item_id: DriveItemId::new("item-root"),
            remote_path: "/".into(),
            display_name: "OneDrive".into(),
            status: PairStatus::Active,
            paused: false,
            delta_token: None,
            errored_reason: None,
            created_at: base_ts(),
            updated_at: base_ts(),
            last_sync_at: None,
            conflict_count: 0,
        }
    }

    /// Insert a minimal `sync_run` row so `file_ops.run_id` FK is satisfied.
    fn insert_sync_run(conn: &Connection, run_id_str: &str, pair_id_str: &str) {
        conn.execute(
            "INSERT INTO sync_runs (id, pair_id, trigger, started_at) VALUES (?, ?, 'scheduled', ?)",
            params![run_id_str, pair_id_str, base_ts().into_inner().to_rfc3339()],
        )
        .expect("insert sync_run");
    }

    fn insert_prerequisites(conn: &Connection) -> (Account, Pair, String) {
        let acct = sample_account();
        let pair = sample_pair(&acct);
        accounts::upsert(conn, &acct).expect("account upsert");
        pairs::upsert(conn, &pair).expect("pair upsert");

        let run_id = Id::<SyncRunTag>::from_ulid(Ulid::from(10u128 << 64)).to_string();
        insert_sync_run(conn, &run_id, &pair.id.to_string());
        (acct, pair, run_id)
    }

    fn sample_op(pair: &Pair, _run_id_str: &str, ulid_seed: u128) -> FileOp {
        use onesync_protocol::id::{FileOpTag, SyncRunTag};
        FileOp {
            id: Id::<FileOpTag>::from_ulid(Ulid::from(ulid_seed)),
            run_id: Id::<SyncRunTag>::from_ulid(Ulid::from(10u128 << 64)),
            pair_id: pair.id,
            relative_path: "Documents/notes.md".parse().expect("rel path"),
            kind: FileOpKind::Upload,
            status: FileOpStatus::Enqueued,
            attempts: 0,
            last_error: None,
            metadata: serde_json::Map::new(),
            enqueued_at: base_ts(),
            started_at: None,
            finished_at: None,
        }
    }

    #[test]
    fn insert_then_in_flight_returns_the_row() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair, run_id) = insert_prerequisites(&conn);
        let _ = run_id;

        let op = sample_op(&pair, &run_id, 100u128 << 64);
        insert(&conn, &op).expect("insert");

        let results = in_flight(&conn, &pair.id).expect("in_flight");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, op.id);
    }

    #[test]
    fn update_status_sets_started_at_on_in_progress() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair, run_id) = insert_prerequisites(&conn);
        let _ = run_id;

        let op = sample_op(&pair, &run_id, 101u128 << 64);
        insert(&conn, &op).expect("insert");

        let clock = TestClock::at(Utc.with_ymd_and_hms(2026, 5, 12, 11, 0, 0).unwrap());
        let now = clock.now();
        update_status(&conn, &op.id, FileOpStatus::InProgress, &now).expect("update");

        // Fetch by inserting a dummy check via in_flight
        let in_flight_ops = in_flight(&conn, &pair.id).expect("in_flight");
        assert_eq!(in_flight_ops.len(), 1);
        assert_eq!(in_flight_ops[0].status, FileOpStatus::InProgress);
        assert_eq!(in_flight_ops[0].started_at, Some(now));
    }

    #[test]
    fn update_status_sets_finished_at_on_success() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair, run_id) = insert_prerequisites(&conn);
        let _ = run_id;

        let op = sample_op(&pair, &run_id, 102u128 << 64);
        insert(&conn, &op).expect("insert");

        let started =
            Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 11, 0, 0).unwrap());
        update_status(&conn, &op.id, FileOpStatus::InProgress, &started).expect("to in_progress");

        let finished =
            Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 11, 5, 0).unwrap());
        update_status(&conn, &op.id, FileOpStatus::Success, &finished).expect("to success");

        // Success ops are no longer in_flight
        let in_flight_ops = in_flight(&conn, &pair.id).expect("in_flight");
        assert!(in_flight_ops.is_empty());

        // Confirm finished_at via direct query
        let finished_at_str: Option<String> = conn
            .query_row(
                "SELECT finished_at FROM file_ops WHERE id = ?",
                params![op.id.to_string()],
                |r| r.get(0),
            )
            .expect("query");
        assert!(finished_at_str.is_some());
        let got_finished = parse_timestamp(&finished_at_str.unwrap()).expect("parse");
        assert_eq!(got_finished, finished);
    }

    #[test]
    fn update_status_preserves_started_at_across_backoff() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair, run_id) = insert_prerequisites(&conn);
        let _ = run_id;

        let op = sample_op(&pair, &run_id, 103u128 << 64);
        insert(&conn, &op).expect("insert");

        let started =
            Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 11, 0, 0).unwrap());
        update_status(&conn, &op.id, FileOpStatus::InProgress, &started).expect("to in_progress");

        // Transition to backoff — started_at should be preserved
        let later = Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 11, 1, 0).unwrap());
        update_status(&conn, &op.id, FileOpStatus::Backoff, &later).expect("to backoff");

        let started_at_str: Option<String> = conn
            .query_row(
                "SELECT started_at FROM file_ops WHERE id = ?",
                params![op.id.to_string()],
                |r| r.get(0),
            )
            .expect("query");
        let got_started = parse_timestamp(&started_at_str.unwrap()).expect("parse");
        // started_at must remain the original started time, not the later backoff time
        assert_eq!(got_started, started);
    }

    #[test]
    fn in_flight_excludes_terminal_states() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair, run_id) = insert_prerequisites(&conn);
        let _ = run_id;

        let op1 = sample_op(&pair, &run_id, 200u128 << 64);
        let mut op2 = sample_op(&pair, &run_id, 201u128 << 64);
        op2.relative_path = "file2.txt".parse().expect("path");
        op2.status = FileOpStatus::Success;
        let mut op3 = sample_op(&pair, &run_id, 202u128 << 64);
        op3.relative_path = "file3.txt".parse().expect("path");
        op3.status = FileOpStatus::Failed;

        insert(&conn, &op1).expect("insert op1");
        insert(&conn, &op2).expect("insert op2");
        insert(&conn, &op3).expect("insert op3");

        let results = in_flight(&conn, &pair.id).expect("in_flight");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, op1.id);
    }
}
