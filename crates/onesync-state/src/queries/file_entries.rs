//! `file_entries` query helpers.

use rusqlite::{Connection, OptionalExtension, params};

use onesync_protocol::{
    enums::{FileKind, FileSyncState},
    file_entry::FileEntry,
    file_side::FileSide,
    id::{FileOpId, PairId},
    path::RelPath,
};

use crate::error::StateStoreError;
use crate::queries::parse_timestamp;

/// Insert or update a file-entry row.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or a side value can't be encoded.
pub fn upsert(conn: &Connection, entry: &FileEntry) -> Result<(), StateStoreError> {
    let local_json = encode_side(entry.local.as_ref())?;
    let remote_json = encode_side(entry.remote.as_ref())?;
    let synced_json = encode_side(entry.synced.as_ref())?;

    conn.execute(
        "INSERT INTO file_entries \
            (pair_id, relative_path, kind, sync_state, local_json, remote_json, synced_json, \
             pending_op_id, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(pair_id, relative_path) DO UPDATE SET \
            kind = excluded.kind, \
            sync_state = excluded.sync_state, \
            local_json = excluded.local_json, \
            remote_json = excluded.remote_json, \
            synced_json = excluded.synced_json, \
            pending_op_id = excluded.pending_op_id, \
            updated_at = excluded.updated_at",
        params![
            entry.pair_id.to_string(),
            entry.relative_path.as_str(),
            kind_to_str(entry.kind),
            state_to_str(entry.sync_state),
            local_json,
            remote_json,
            synced_json,
            entry.pending_op_id.as_ref().map(ToString::to_string),
            entry.updated_at.into_inner().to_rfc3339(),
        ],
    )
    .map(|_| ())
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Fetch a file entry by `(pair_id, relative_path)`.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or a row can't be decoded.
pub fn get(
    conn: &Connection,
    pair: &PairId,
    path: &RelPath,
) -> Result<Option<FileEntry>, StateStoreError> {
    conn.query_row(
        "SELECT pair_id, relative_path, kind, sync_state, local_json, remote_json, synced_json, \
                pending_op_id, updated_at \
         FROM file_entries WHERE pair_id = ? AND relative_path = ?",
        params![pair.to_string(), path.as_str()],
        row_to_entry,
    )
    .optional()
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Fetch up to `limit` non-clean entries for `pair`, ordered by `updated_at`.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or rows can't be decoded.
pub fn dirty(
    conn: &Connection,
    pair: &PairId,
    limit: usize,
) -> Result<Vec<FileEntry>, StateStoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT pair_id, relative_path, kind, sync_state, local_json, remote_json, \
                    synced_json, pending_op_id, updated_at \
             FROM file_entries \
             WHERE pair_id = ? AND sync_state <> 'clean' \
             ORDER BY updated_at \
             LIMIT ?",
        )
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    let rows = stmt
        .query_map(
            params![pair.to_string(), i64::try_from(limit).unwrap_or(i64::MAX)],
            row_to_entry,
        )
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    rows.map(|r| r.map_err(|e| StateStoreError::Sqlite(e.to_string())))
        .collect()
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileEntry> {
    let pair_id_str: String = row.get(0)?;
    let rel_path_str: String = row.get(1)?;
    let kind_str: String = row.get(2)?;
    let state_str: String = row.get(3)?;
    let local_json: Option<String> = row.get(4)?;
    let remote_json: Option<String> = row.get(5)?;
    let synced_json: Option<String> = row.get(6)?;
    let pending_op_id_str: Option<String> = row.get(7)?;
    let updated_at_str: String = row.get(8)?;

    Ok(FileEntry {
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
        kind: kind_from_str(&kind_str)?,
        sync_state: state_from_str(&state_str)?,
        local: decode_side(local_json.as_deref())?,
        remote: decode_side(remote_json.as_deref())?,
        synced: decode_side(synced_json.as_deref())?,
        pending_op_id: pending_op_id_str
            .map(|s| {
                s.parse::<FileOpId>()
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            })
            .transpose()?,
        updated_at: parse_timestamp(&updated_at_str)?,
    })
}

fn encode_side(side: Option<&FileSide>) -> Result<Option<String>, StateStoreError> {
    side.map(|s| {
        serde_json::to_string(s).map_err(|e| StateStoreError::Sqlite(format!("encode side: {e}")))
    })
    .transpose()
}

fn decode_side(json: Option<&str>) -> rusqlite::Result<Option<FileSide>> {
    json.map(|s| {
        serde_json::from_str(s).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    })
    .transpose()
}

const fn kind_to_str(k: FileKind) -> &'static str {
    match k {
        FileKind::File => "file",
        FileKind::Directory => "directory",
    }
}

fn kind_from_str(s: &str) -> rusqlite::Result<FileKind> {
    match s {
        "file" => Ok(FileKind::File),
        "directory" => Ok(FileKind::Directory),
        other => Err(rusqlite::Error::InvalidColumnType(
            2,
            format!("unknown file kind: {other}"),
            rusqlite::types::Type::Text,
        )),
    }
}

const fn state_to_str(s: FileSyncState) -> &'static str {
    match s {
        FileSyncState::Clean => "clean",
        FileSyncState::Dirty => "dirty",
        FileSyncState::PendingUpload => "pending_upload",
        FileSyncState::PendingDownload => "pending_download",
        FileSyncState::PendingConflict => "pending_conflict",
        FileSyncState::InFlight => "in_flight",
    }
}

fn state_from_str(s: &str) -> rusqlite::Result<FileSyncState> {
    match s {
        "clean" => Ok(FileSyncState::Clean),
        "dirty" => Ok(FileSyncState::Dirty),
        "pending_upload" => Ok(FileSyncState::PendingUpload),
        "pending_download" => Ok(FileSyncState::PendingDownload),
        "pending_conflict" => Ok(FileSyncState::PendingConflict),
        "in_flight" => Ok(FileSyncState::InFlight),
        other => Err(rusqlite::Error::InvalidColumnType(
            3,
            format!("unknown file sync state: {other}"),
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
    use onesync_protocol::{
        account::Account,
        enums::{AccountKind, PairStatus},
        id::{AccountTag, Id, PairTag},
        pair::Pair,
        primitives::{ContentHash, DriveId, DriveItemId, ETag, KeychainRef, Timestamp},
    };
    use tempfile::TempDir;
    use ulid::Ulid;

    fn fresh_db() -> (TempDir, crate::connection::ConnectionPool) {
        let tmp = TempDir::new().expect("tmpdir");
        let pool = open(&tmp.path().join("t.sqlite")).expect("open");
        (tmp, pool)
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
            created_at: Timestamp::from_datetime(
                Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap(),
            ),
            updated_at: Timestamp::from_datetime(
                Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap(),
            ),
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
            created_at: Timestamp::from_datetime(
                Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap(),
            ),
            updated_at: Timestamp::from_datetime(
                Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap(),
            ),
            last_sync_at: None,
            conflict_count: 0,
        }
    }

    fn insert_prerequisites(conn: &Connection) -> (Account, Pair) {
        let acct = sample_account();
        let pair = sample_pair(&acct);
        accounts::upsert(conn, &acct).expect("account upsert");
        pairs::upsert(conn, &pair).expect("pair upsert");
        (acct, pair)
    }

    fn make_side(size: u64, hash_byte: u8, mtime_offset_secs: i64) -> FileSide {
        FileSide {
            kind: FileKind::File,
            size_bytes: size,
            content_hash: Some(ContentHash::from_bytes([hash_byte; 32])),
            mtime: Timestamp::from_datetime(
                Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap()
                    + chrono::Duration::seconds(mtime_offset_secs),
            ),
            etag: None,
            remote_item_id: None,
        }
    }

    fn make_remote_side(size: u64, hash_byte: u8, mtime_offset_secs: i64) -> FileSide {
        FileSide {
            kind: FileKind::File,
            size_bytes: size,
            content_hash: Some(ContentHash::from_bytes([hash_byte; 32])),
            mtime: Timestamp::from_datetime(
                Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap()
                    + chrono::Duration::seconds(mtime_offset_secs),
            ),
            etag: Some(ETag::new("etag-abc")),
            remote_item_id: Some(DriveItemId::new("item-123")),
        }
    }

    fn base_entry(pair: &Pair) -> FileEntry {
        FileEntry {
            pair_id: pair.id,
            relative_path: "Documents/notes.md".parse().expect("rel path"),
            kind: FileKind::File,
            sync_state: FileSyncState::Clean,
            local: None,
            remote: None,
            synced: None,
            pending_op_id: None,
            updated_at: Timestamp::from_datetime(
                Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap(),
            ),
        }
    }

    #[test]
    fn upsert_then_get_round_trips_with_all_sides() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        let entry = FileEntry {
            pair_id: pair.id,
            relative_path: "Documents/notes.md".parse().expect("rel path"),
            kind: FileKind::File,
            sync_state: FileSyncState::Clean,
            local: Some(make_side(1024, 0xAA, 0)),
            remote: Some(make_remote_side(1024, 0xAA, 10)),
            synced: Some(make_side(512, 0xBB, -100)),
            pending_op_id: None,
            updated_at: Timestamp::from_datetime(
                Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap(),
            ),
        };

        upsert(&conn, &entry).expect("upsert");
        let back = get(&conn, &pair.id, &entry.relative_path)
            .expect("get")
            .expect("present");
        assert_eq!(back, entry);
    }

    #[test]
    fn upsert_with_partial_sides_round_trips() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        let entry = FileEntry {
            local: Some(make_side(256, 0x01, 0)),
            remote: None,
            synced: None,
            ..base_entry(&pair)
        };

        upsert(&conn, &entry).expect("upsert");
        let back = get(&conn, &pair.id, &entry.relative_path)
            .expect("get")
            .expect("present");
        assert_eq!(back.local, entry.local);
        assert!(back.remote.is_none());
        assert!(back.synced.is_none());
    }

    #[test]
    fn dirty_returns_only_non_clean_entries() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        let clean = FileEntry {
            relative_path: "clean.txt".parse().expect("path"),
            sync_state: FileSyncState::Clean,
            ..base_entry(&pair)
        };
        let dirty_entry = FileEntry {
            relative_path: "dirty.txt".parse().expect("path"),
            sync_state: FileSyncState::Dirty,
            ..base_entry(&pair)
        };
        let pending = FileEntry {
            relative_path: "pending.txt".parse().expect("path"),
            sync_state: FileSyncState::PendingUpload,
            ..base_entry(&pair)
        };

        upsert(&conn, &clean).expect("upsert clean");
        upsert(&conn, &dirty_entry).expect("upsert dirty");
        upsert(&conn, &pending).expect("upsert pending");

        let results = dirty(&conn, &pair.id, 10).expect("dirty");
        assert_eq!(results.len(), 2);
        let states: Vec<FileSyncState> = results.iter().map(|e| e.sync_state).collect();
        assert!(!states.contains(&FileSyncState::Clean));
        assert!(states.contains(&FileSyncState::Dirty));
        assert!(states.contains(&FileSyncState::PendingUpload));
    }

    #[test]
    fn dirty_respects_limit() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        for i in 0..5_u8 {
            let entry = FileEntry {
                relative_path: format!("file{i}.txt").parse().expect("path"),
                sync_state: FileSyncState::Dirty,
                ..base_entry(&pair)
            };
            upsert(&conn, &entry).expect("upsert");
        }

        let results = dirty(&conn, &pair.id, 3).expect("dirty");
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn get_returns_none_for_unknown_path() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        let path: RelPath = "nonexistent.txt".parse().expect("path");
        let result = get(&conn, &pair.id, &path).expect("get");
        assert!(result.is_none());
    }
}
