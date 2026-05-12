//! Pair queries.

use rusqlite::{Connection, OptionalExtension, params};

use onesync_protocol::{
    enums::PairStatus,
    id::PairId,
    pair::Pair,
    primitives::{DeltaCursor, DriveItemId},
};

use crate::error::StateStoreError;
use crate::queries::parse_timestamp;

/// Insert or update a pair row.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the underlying SQL call fails.
pub fn upsert(conn: &Connection, pair: &Pair) -> Result<(), StateStoreError> {
    conn.execute(
        "INSERT INTO pairs \
            (id, account_id, local_path, remote_item_id, remote_path, display_name, \
             status, paused, delta_token, errored_reason, created_at, updated_at, \
             last_sync_at, conflict_count) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
            account_id = excluded.account_id, \
            local_path = excluded.local_path, \
            remote_item_id = excluded.remote_item_id, \
            remote_path = excluded.remote_path, \
            display_name = excluded.display_name, \
            status = excluded.status, \
            paused = excluded.paused, \
            delta_token = excluded.delta_token, \
            errored_reason = excluded.errored_reason, \
            updated_at = excluded.updated_at, \
            last_sync_at = excluded.last_sync_at, \
            conflict_count = excluded.conflict_count",
        params![
            pair.id.to_string(),
            pair.account_id.to_string(),
            pair.local_path.as_str(),
            pair.remote_item_id.as_str(),
            pair.remote_path,
            pair.display_name,
            status_to_str(pair.status),
            i32::from(pair.paused),
            pair.delta_token.as_ref().map(DeltaCursor::as_str),
            pair.errored_reason,
            pair.created_at.into_inner().to_rfc3339(),
            pair.updated_at.into_inner().to_rfc3339(),
            pair.last_sync_at.map(|ts| ts.into_inner().to_rfc3339()),
            i64::from(pair.conflict_count),
        ],
    )
    .map(|_| ())
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Fetch a pair by id.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or a row can't be decoded.
pub fn get(conn: &Connection, id: &PairId) -> Result<Option<Pair>, StateStoreError> {
    conn.query_row(
        "SELECT id, account_id, local_path, remote_item_id, remote_path, display_name, \
                status, paused, delta_token, errored_reason, created_at, updated_at, \
                last_sync_at, conflict_count \
         FROM pairs WHERE id = ?",
        params![id.to_string()],
        row_to_pair,
    )
    .optional()
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Fetch all non-removed pairs ordered by id.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or rows can't be decoded.
pub fn active(conn: &Connection) -> Result<Vec<Pair>, StateStoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, account_id, local_path, remote_item_id, remote_path, display_name, \
                    status, paused, delta_token, errored_reason, created_at, updated_at, \
                    last_sync_at, conflict_count \
             FROM pairs WHERE status <> 'removed' ORDER BY id",
        )
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    let rows = stmt
        .query_map([], row_to_pair)
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    rows.map(|r| r.map_err(|e| StateStoreError::Sqlite(e.to_string())))
        .collect()
}

fn row_to_pair(row: &rusqlite::Row<'_>) -> rusqlite::Result<Pair> {
    let id_str: String = row.get(0)?;
    let account_id_str: String = row.get(1)?;
    let local_path_str: String = row.get(2)?;
    let status_str: String = row.get(6)?;
    let paused_int: i32 = row.get(7)?;
    let delta_token_opt: Option<String> = row.get(8)?;
    let created_at_str: String = row.get(10)?;
    let updated_at_str: String = row.get(11)?;
    let last_sync_at_opt: Option<String> = row.get(12)?;
    let conflict_count_i64: i64 = row.get(13)?;

    Ok(Pair {
        id: id_str
            .parse()
            .map_err(|e: onesync_protocol::id::IdParseError| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            })?,
        account_id: account_id_str
            .parse()
            .map_err(|e: onesync_protocol::id::IdParseError| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            })?,
        local_path: local_path_str.parse().map_err(
            |e: onesync_protocol::path::PathParseError| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            },
        )?,
        remote_item_id: DriveItemId::new(row.get::<_, String>(3)?),
        remote_path: row.get(4)?,
        display_name: row.get(5)?,
        status: status_from_str(&status_str)?,
        paused: paused_int != 0,
        delta_token: delta_token_opt.map(DeltaCursor::new),
        errored_reason: row.get(9)?,
        created_at: parse_timestamp(&created_at_str)?,
        updated_at: parse_timestamp(&updated_at_str)?,
        last_sync_at: last_sync_at_opt.map(|s| parse_timestamp(&s)).transpose()?,
        conflict_count: u32::try_from(conflict_count_i64).unwrap_or(0),
    })
}

const fn status_to_str(s: PairStatus) -> &'static str {
    match s {
        PairStatus::Initializing => "initializing",
        PairStatus::Active => "active",
        PairStatus::Paused => "paused",
        PairStatus::Errored => "errored",
        PairStatus::Removed => "removed",
    }
}

fn status_from_str(s: &str) -> rusqlite::Result<PairStatus> {
    match s {
        "initializing" => Ok(PairStatus::Initializing),
        "active" => Ok(PairStatus::Active),
        "paused" => Ok(PairStatus::Paused),
        "errored" => Ok(PairStatus::Errored),
        "removed" => Ok(PairStatus::Removed),
        other => Err(rusqlite::Error::InvalidColumnType(
            6,
            format!("unknown pair status: {other}"),
            rusqlite::types::Type::Text,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open;
    use crate::queries::accounts;
    use chrono::{TimeZone, Utc};
    use onesync_protocol::{
        account::Account,
        enums::AccountKind,
        id::{AccountTag, Id, PairTag},
        primitives::{DriveId, KeychainRef, Timestamp},
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

    fn sample_pair(account: &Account, ulid_seed: u128, local_path: &str) -> Pair {
        Pair {
            id: Id::<PairTag>::from_ulid(Ulid::from(ulid_seed)),
            account_id: account.id,
            local_path: local_path.parse().expect("abs path"),
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

    #[test]
    fn upsert_then_get_round_trips() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let acct = sample_account();
        accounts::upsert(&conn, &acct).expect("account upsert");
        let pair = sample_pair(&acct, 2u128 << 64, "/Users/alice/OneDrive");
        upsert(&conn, &pair).expect("pair upsert");
        let back = get(&conn, &pair.id).expect("get").expect("present");
        assert_eq!(back, pair);
    }

    #[test]
    fn active_excludes_removed_pairs() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let acct = sample_account();
        accounts::upsert(&conn, &acct).expect("account upsert");

        let mut pair1 = sample_pair(&acct, 2u128 << 64, "/Users/alice/OneDrive");
        let mut pair2 = sample_pair(&acct, 3u128 << 64, "/Users/alice/Work");
        pair2.remote_item_id = DriveItemId::new("item-work");

        upsert(&conn, &pair1).expect("upsert pair1");
        upsert(&conn, &pair2).expect("upsert pair2");

        // Update pair2 to Removed status
        pair2.status = PairStatus::Removed;
        upsert(&conn, &pair2).expect("upsert pair2 removed");

        let active_pairs = active(&conn).expect("active");
        assert_eq!(active_pairs.len(), 1);
        assert_eq!(active_pairs[0].id, pair1.id);

        // Also confirm pair1 is still active
        pair1.status = PairStatus::Active;
        assert_eq!(active_pairs[0].status, PairStatus::Active);
    }

    #[test]
    fn unique_local_path_constraint_fires() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let acct = sample_account();
        accounts::upsert(&conn, &acct).expect("account upsert");

        let pair1 = sample_pair(&acct, 2u128 << 64, "/Users/alice/OneDrive");
        // Different id, same local_path, both Active
        let pair2 = sample_pair(&acct, 3u128 << 64, "/Users/alice/OneDrive");

        upsert(&conn, &pair1).expect("first upsert succeeds");
        let result = upsert(&conn, &pair2);
        assert!(
            result.is_err(),
            "second upsert with same local_path should fail"
        );
        assert!(matches!(result, Err(StateStoreError::Sqlite(_))));
    }
}
