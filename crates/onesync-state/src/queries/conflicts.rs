//! `conflicts` query helpers.

use rusqlite::{Connection, params};

use onesync_protocol::{
    conflict::Conflict,
    enums::{ConflictResolution, ConflictSide},
    file_side::FileSide,
    id::PairId,
};

use crate::error::StateStoreError;
use crate::queries::parse_timestamp;

/// Insert a new conflict record.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or values can't be encoded.
pub fn insert(conn: &Connection, c: &Conflict) -> Result<(), StateStoreError> {
    let local_side_json = encode_side(&c.local_side)?;
    let remote_side_json = encode_side(&c.remote_side)?;

    conn.execute(
        "INSERT INTO conflicts \
            (id, pair_id, relative_path, winner, loser_relative_path, \
             local_side_json, remote_side_json, detected_at, resolved_at, resolution, note) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            c.id.to_string(),
            c.pair_id.to_string(),
            c.relative_path.as_str(),
            conflict_side_to_str(c.winner),
            c.loser_relative_path.as_str(),
            local_side_json,
            remote_side_json,
            c.detected_at.into_inner().to_rfc3339(),
            c.resolved_at.map(|ts| ts.into_inner().to_rfc3339()),
            c.resolution.map(conflict_resolution_to_str),
            c.note.as_deref(),
        ],
    )
    .map(|_| ())
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Fetch all unresolved conflicts for `pair`, ordered by `detected_at`.
///
/// Uses the partial index `conflicts_pair_unresolved_idx`.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or rows can't be decoded.
pub fn unresolved(conn: &Connection, pair: &PairId) -> Result<Vec<Conflict>, StateStoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, pair_id, relative_path, winner, loser_relative_path, \
                    local_side_json, remote_side_json, detected_at, resolved_at, resolution, note \
             FROM conflicts \
             WHERE pair_id = ? AND resolved_at IS NULL \
             ORDER BY detected_at",
        )
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    let rows = stmt
        .query_map(params![pair.to_string()], row_to_conflict)
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    rows.map(|r| r.map_err(|e| StateStoreError::Sqlite(e.to_string())))
        .collect()
}

fn row_to_conflict(row: &rusqlite::Row<'_>) -> rusqlite::Result<Conflict> {
    let id_str: String = row.get(0)?;
    let pair_id_str: String = row.get(1)?;
    let rel_path_str: String = row.get(2)?;
    let winner_str: String = row.get(3)?;
    let loser_path_str: String = row.get(4)?;
    let local_side_json: String = row.get(5)?;
    let remote_side_json: String = row.get(6)?;
    let detected_at_str: String = row.get(7)?;
    let resolved_at_opt: Option<String> = row.get(8)?;
    let resolution_opt: Option<String> = row.get(9)?;
    let note: Option<String> = row.get(10)?;

    Ok(Conflict {
        id: id_str
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
        winner: conflict_side_from_str(&winner_str)?,
        loser_relative_path: loser_path_str.parse().map_err(
            |e: onesync_protocol::path::PathParseError| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            },
        )?,
        local_side: decode_side(&local_side_json)?,
        remote_side: decode_side(&remote_side_json)?,
        detected_at: parse_timestamp(&detected_at_str)?,
        resolved_at: resolved_at_opt.map(|s| parse_timestamp(&s)).transpose()?,
        resolution: resolution_opt
            .map(|s| conflict_resolution_from_str(&s))
            .transpose()?,
        note,
    })
}

fn encode_side(side: &FileSide) -> Result<String, StateStoreError> {
    serde_json::to_string(side)
        .map_err(|e| StateStoreError::Sqlite(format!("encode file side: {e}")))
}

fn decode_side(json: &str) -> rusqlite::Result<FileSide> {
    serde_json::from_str(json).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
}

const fn conflict_side_to_str(s: ConflictSide) -> &'static str {
    match s {
        ConflictSide::Local => "local",
        ConflictSide::Remote => "remote",
    }
}

fn conflict_side_from_str(s: &str) -> rusqlite::Result<ConflictSide> {
    match s {
        "local" => Ok(ConflictSide::Local),
        "remote" => Ok(ConflictSide::Remote),
        other => Err(rusqlite::Error::InvalidColumnType(
            3,
            format!("unknown conflict side: {other}"),
            rusqlite::types::Type::Text,
        )),
    }
}

const fn conflict_resolution_to_str(r: ConflictResolution) -> &'static str {
    match r {
        ConflictResolution::Auto => "auto",
        ConflictResolution::Manual => "manual",
    }
}

fn conflict_resolution_from_str(s: &str) -> rusqlite::Result<ConflictResolution> {
    match s {
        "auto" => Ok(ConflictResolution::Auto),
        "manual" => Ok(ConflictResolution::Manual),
        other => Err(rusqlite::Error::InvalidColumnType(
            9,
            format!("unknown conflict resolution: {other}"),
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
        enums::{AccountKind, FileKind, PairStatus},
        id::{AccountTag, ConflictTag, Id, PairTag},
        pair::Pair,
        primitives::{ContentHash, DriveId, DriveItemId, KeychainRef, Timestamp},
    };
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

    fn insert_prerequisites(conn: &Connection) -> (Account, Pair) {
        let acct = sample_account();
        let pair = sample_pair(&acct);
        accounts::upsert(conn, &acct).expect("account upsert");
        pairs::upsert(conn, &pair).expect("pair upsert");
        (acct, pair)
    }

    fn make_file_side(size: u64, hash_byte: u8) -> FileSide {
        FileSide {
            kind: FileKind::File,
            size_bytes: size,
            content_hash: Some(ContentHash::from_bytes([hash_byte; 32])),
            mtime: base_ts(),
            etag: None,
            remote_item_id: None,
        }
    }

    fn sample_conflict(pair: &Pair, seed: u128) -> Conflict {
        Conflict {
            id: Id::<ConflictTag>::from_ulid(Ulid::from(seed)),
            pair_id: pair.id,
            relative_path: "Documents/report.docx".parse().expect("rel path"),
            winner: ConflictSide::Local,
            loser_relative_path: "Documents/report (conflict 2026-05-12).docx"
                .parse()
                .expect("rel path"),
            local_side: make_file_side(1024, 0xAA),
            remote_side: make_file_side(2048, 0xBB),
            detected_at: base_ts(),
            resolved_at: None,
            resolution: None,
            note: None,
        }
    }

    #[test]
    fn insert_then_unresolved_returns_the_conflict() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        let conflict = sample_conflict(&pair, 100u128 << 64);
        insert(&conn, &conflict).expect("insert");

        let results = unresolved(&conn, &pair.id).expect("unresolved");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, conflict.id);
        assert_eq!(results[0].winner, ConflictSide::Local);
    }

    #[test]
    fn unresolved_excludes_resolved_conflicts() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        let c1 = sample_conflict(&pair, 101u128 << 64);
        let mut c2 = sample_conflict(&pair, 102u128 << 64);
        c2.relative_path = "Documents/other.docx".parse().expect("rel path");
        c2.loser_relative_path = "Documents/other (conflict).docx".parse().expect("rel path");
        // Mark c2 as resolved
        c2.resolved_at = Some(Timestamp::from_datetime(
            Utc.with_ymd_and_hms(2026, 5, 12, 11, 0, 0).unwrap(),
        ));
        c2.resolution = Some(ConflictResolution::Auto);

        insert(&conn, &c1).expect("insert c1");
        insert(&conn, &c2).expect("insert c2");

        let results = unresolved(&conn, &pair.id).expect("unresolved");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, c1.id);
    }

    #[test]
    fn insert_round_trips_with_both_file_sides() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        let local_side = make_file_side(512, 0x11);
        let remote_side = make_file_side(768, 0x22);

        let conflict = Conflict {
            id: Id::<ConflictTag>::from_ulid(Ulid::from(103u128 << 64)),
            pair_id: pair.id,
            relative_path: "data/file.bin".parse().expect("rel path"),
            winner: ConflictSide::Remote,
            loser_relative_path: "data/file (conflict).bin".parse().expect("rel path"),
            local_side: local_side.clone(),
            remote_side: remote_side.clone(),
            detected_at: base_ts(),
            resolved_at: None,
            resolution: None,
            note: Some("operator note".to_owned()),
        };

        insert(&conn, &conflict).expect("insert");

        let results = unresolved(&conn, &pair.id).expect("unresolved");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].local_side, local_side);
        assert_eq!(results[0].remote_side, remote_side);
        assert_eq!(results[0].winner, ConflictSide::Remote);
        assert_eq!(results[0].note.as_deref(), Some("operator note"));
    }
}
