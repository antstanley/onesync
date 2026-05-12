//! `audit_events` query helpers.

use rusqlite::{Connection, params};

use onesync_protocol::{audit::AuditEvent, enums::AuditLevel, id::PairId, primitives::Timestamp};

use crate::error::StateStoreError;
use crate::queries::parse_timestamp;

/// Append a new audit event (append-only; never updated).
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or the payload can't be encoded.
pub fn append(conn: &Connection, evt: &AuditEvent) -> Result<(), StateStoreError> {
    let payload_json = if evt.payload.is_empty() {
        "{}".to_owned()
    } else {
        serde_json::to_string(&evt.payload)
            .map_err(|e| StateStoreError::Sqlite(format!("encode audit payload: {e}")))?
    };

    conn.execute(
        "INSERT INTO audit_events (id, ts, level, kind, pair_id, payload_json) \
         VALUES (?, ?, ?, ?, ?, ?)",
        params![
            evt.id.to_string(),
            evt.ts.into_inner().to_rfc3339(),
            audit_level_to_str(evt.level),
            evt.kind.as_str(),
            evt.pair_id.as_ref().map(ToString::to_string),
            payload_json,
        ],
    )
    .map(|_| ())
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Fetch the `limit` most-recent audit events, ordered by `ts DESC`.
///
/// Uses the index `audit_events_ts_idx`.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or rows can't be decoded.
pub fn recent(conn: &Connection, limit: usize) -> Result<Vec<AuditEvent>, StateStoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, ts, level, kind, pair_id, payload_json \
             FROM audit_events \
             ORDER BY ts DESC \
             LIMIT ?",
        )
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    let rows = stmt
        .query_map(
            params![i64::try_from(limit).unwrap_or(i64::MAX)],
            row_to_event,
        )
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    rows.map(|r| r.map_err(|e| StateStoreError::Sqlite(e.to_string())))
        .collect()
}

/// Search audit events within a time window, with optional level and pair filters.
///
/// Always filters `ts >= from_ts AND ts <= to_ts`. Optionally also filters by `level`
/// and/or `pair_id`. Results are ordered `ts DESC` and capped at `limit`.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or rows can't be decoded.
pub fn search(
    conn: &Connection,
    from_ts: &Timestamp,
    to_ts: &Timestamp,
    level: Option<AuditLevel>,
    pair: Option<&PairId>,
    limit: usize,
) -> Result<Vec<AuditEvent>, StateStoreError> {
    let mut sql = "SELECT id, ts, level, kind, pair_id, payload_json \
                   FROM audit_events \
                   WHERE ts >= ? AND ts <= ?"
        .to_owned();

    // Collect bind values after the two mandatory timestamps.
    let mut extra_binds: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if let Some(lvl) = level {
        sql.push_str(" AND level = ?");
        extra_binds.push(Box::new(audit_level_to_str(lvl)));
    }
    if let Some(p) = pair {
        sql.push_str(" AND pair_id = ?");
        extra_binds.push(Box::new(p.to_string()));
    }

    sql.push_str(" ORDER BY ts DESC LIMIT ?");

    let from_str = from_ts.into_inner().to_rfc3339();
    let to_str = to_ts.into_inner().to_rfc3339();
    let limit_i64 = i64::try_from(limit).unwrap_or(i64::MAX);

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    // Build a unified params slice: from_str, to_str, extras..., limit.
    let mut all_params: Vec<&dyn rusqlite::types::ToSql> = vec![&from_str, &to_str];
    for b in &extra_binds {
        all_params.push(b.as_ref());
    }
    all_params.push(&limit_i64);

    let rows = stmt
        .query_map(all_params.as_slice(), row_to_event)
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    rows.map(|r| r.map_err(|e| StateStoreError::Sqlite(e.to_string())))
        .collect()
}

fn row_to_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEvent> {
    let id_str: String = row.get(0)?;
    let ts_str: String = row.get(1)?;
    let level_str: String = row.get(2)?;
    let kind: String = row.get(3)?;
    let pair_id_str: Option<String> = row.get(4)?;
    let payload_json: String = row.get(5)?;

    let payload: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&payload_json)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

    Ok(AuditEvent {
        id: id_str
            .parse()
            .map_err(|e: onesync_protocol::id::IdParseError| {
                rusqlite::Error::ToSqlConversionFailure(Box::new(e))
            })?,
        ts: parse_timestamp(&ts_str)?,
        level: audit_level_from_str(&level_str)?,
        kind,
        pair_id: pair_id_str
            .map(|s| {
                s.parse::<PairId>()
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            })
            .transpose()?,
        payload,
    })
}

const fn audit_level_to_str(l: AuditLevel) -> &'static str {
    match l {
        AuditLevel::Info => "info",
        AuditLevel::Warn => "warn",
        AuditLevel::Error => "error",
    }
}

fn audit_level_from_str(s: &str) -> rusqlite::Result<AuditLevel> {
    match s {
        "info" => Ok(AuditLevel::Info),
        "warn" => Ok(AuditLevel::Warn),
        "error" => Ok(AuditLevel::Error),
        other => Err(rusqlite::Error::InvalidColumnType(
            2,
            format!("unknown audit level: {other}"),
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
        id::{AccountTag, AuditTag, Id, PairTag},
        pair::Pair,
        primitives::{DriveId, DriveItemId, KeychainRef, Timestamp},
    };
    use onesync_time::fakes::TestClock;
    use std::time::Duration;
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

    fn make_event(
        seed: u128,
        ts: Timestamp,
        level: AuditLevel,
        pair_id: Option<PairId>,
    ) -> AuditEvent {
        AuditEvent {
            id: Id::<AuditTag>::from_ulid(Ulid::from(seed)),
            ts,
            level,
            kind: "test_event".to_owned(),
            pair_id,
            payload: serde_json::Map::new(),
        }
    }

    #[test]
    fn append_then_recent_returns_the_event() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");

        let evt = make_event(100u128 << 64, base_ts(), AuditLevel::Info, None);
        append(&conn, &evt).expect("append");

        let results = recent(&conn, 10).expect("recent");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, evt.id);
        assert_eq!(results[0].level, AuditLevel::Info);
        assert_eq!(results[0].kind, "test_event");
    }

    #[test]
    fn recent_orders_by_ts_desc_and_respects_limit() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");

        let clock = TestClock::at(Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap());

        let mut events = Vec::new();
        for seed in 0..5u128 {
            let ts = clock.now();
            clock.advance(Duration::from_secs(1));
            let evt = make_event((seed + 200) << 64, ts, AuditLevel::Info, None);
            append(&conn, &evt).expect("append");
            events.push(evt);
        }

        let results = recent(&conn, 3).expect("recent");
        assert_eq!(results.len(), 3);
        // Most recent first — seed 204 was latest
        assert_eq!(results[0].id, events[4].id);
        assert_eq!(results[1].id, events[3].id);
        assert_eq!(results[2].id, events[2].id);
    }

    #[test]
    fn search_filters_by_time_window() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");

        let t0 = Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap());
        let t1 = Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 11, 0, 0).unwrap());
        let t2 = Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 12, 0, 0).unwrap());
        let t3 = Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 13, 0, 0).unwrap());

        append(
            &conn,
            &make_event(300u128 << 64, t0, AuditLevel::Info, None),
        )
        .expect("e0");
        append(
            &conn,
            &make_event(301u128 << 64, t1, AuditLevel::Info, None),
        )
        .expect("e1");
        append(
            &conn,
            &make_event(302u128 << 64, t2, AuditLevel::Info, None),
        )
        .expect("e2");
        append(
            &conn,
            &make_event(303u128 << 64, t3, AuditLevel::Info, None),
        )
        .expect("e3");

        // Window [t1, t2] should return events at t1 and t2
        let results = search(&conn, &t1, &t2, None, None, 100).expect("search");
        assert_eq!(results.len(), 2);
        // Ordered desc: t2 first, t1 second
        assert_eq!(results[0].ts, t2, "first result should be at t2");
        assert_eq!(results[1].ts, t1, "second result should be at t1");
    }

    #[test]
    fn search_filters_by_level() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");

        let t0 = base_ts();
        append(
            &conn,
            &make_event(400u128 << 64, t0, AuditLevel::Info, None),
        )
        .expect("info");
        append(
            &conn,
            &make_event(401u128 << 64, t0, AuditLevel::Warn, None),
        )
        .expect("warn");
        append(
            &conn,
            &make_event(402u128 << 64, t0, AuditLevel::Error, None),
        )
        .expect("error");

        let far_future =
            Timestamp::from_datetime(Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap());
        let results =
            search(&conn, &t0, &far_future, Some(AuditLevel::Warn), None, 100).expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].level, AuditLevel::Warn);
    }

    #[test]
    fn search_filters_by_pair_id() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        let t0 = base_ts();
        let far_future =
            Timestamp::from_datetime(Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap());

        // Event linked to the pair
        append(
            &conn,
            &make_event(500u128 << 64, t0, AuditLevel::Info, Some(pair.id)),
        )
        .expect("with pair");
        // Event with no pair association
        append(
            &conn,
            &make_event(501u128 << 64, t0, AuditLevel::Info, None),
        )
        .expect("no pair");

        let results = search(&conn, &t0, &far_future, None, Some(&pair.id), 100).expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].pair_id, Some(pair.id));
    }
}
