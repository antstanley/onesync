//! `sync_runs` query helpers.

use rusqlite::{Connection, params};

use onesync_protocol::{
    enums::{RunOutcome, RunTrigger},
    id::PairId,
    sync_run::SyncRun,
};

use crate::error::StateStoreError;
use crate::queries::parse_timestamp;

/// Insert or update a sync-run row.
///
/// A run may be inserted at start (with `finished_at = None`) and updated at finish. The
/// upsert on `id` updates the mutable finish-time fields while leaving `pair_id`, `trigger`,
/// and `started_at` untouched.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or values can't be encoded.
pub fn record(conn: &Connection, run: &SyncRun) -> Result<(), StateStoreError> {
    conn.execute(
        "INSERT INTO sync_runs \
            (id, pair_id, trigger, started_at, finished_at, \
             local_ops, remote_ops, bytes_uploaded, bytes_downloaded, outcome, outcome_detail) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
            finished_at      = excluded.finished_at, \
            local_ops        = excluded.local_ops, \
            remote_ops       = excluded.remote_ops, \
            bytes_uploaded   = excluded.bytes_uploaded, \
            bytes_downloaded = excluded.bytes_downloaded, \
            outcome          = excluded.outcome, \
            outcome_detail   = excluded.outcome_detail",
        params![
            run.id.to_string(),
            run.pair_id.to_string(),
            run_trigger_to_str(run.trigger),
            run.started_at.into_inner().to_rfc3339(),
            run.finished_at.map(|ts| ts.into_inner().to_rfc3339()),
            i64::from(run.local_ops),
            i64::from(run.remote_ops),
            i64::try_from(run.bytes_uploaded).unwrap_or(i64::MAX),
            i64::try_from(run.bytes_downloaded).unwrap_or(i64::MAX),
            run.outcome.map(run_outcome_to_str),
            run.outcome_detail.as_deref(),
        ],
    )
    .map(|_| ())
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Fetch the `limit` most-recent sync runs for `pair`, ordered by `started_at DESC`.
///
/// Uses the index `sync_runs_pair_started_idx`.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` if the SQL call fails or rows can't be decoded.
pub fn recent(
    conn: &Connection,
    pair: &PairId,
    limit: usize,
) -> Result<Vec<SyncRun>, StateStoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, pair_id, trigger, started_at, finished_at, \
                    local_ops, remote_ops, bytes_uploaded, bytes_downloaded, outcome, outcome_detail \
             FROM sync_runs \
             WHERE pair_id = ? \
             ORDER BY started_at DESC \
             LIMIT ?",
        )
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    let rows = stmt
        .query_map(
            params![pair.to_string(), i64::try_from(limit).unwrap_or(i64::MAX)],
            row_to_run,
        )
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;

    rows.map(|r| r.map_err(|e| StateStoreError::Sqlite(e.to_string())))
        .collect()
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<SyncRun> {
    let id_str: String = row.get(0)?;
    let pair_id_str: String = row.get(1)?;
    let trigger_str: String = row.get(2)?;
    let started_at_str: String = row.get(3)?;
    let finished_at_opt: Option<String> = row.get(4)?;
    let local_ops_i64: i64 = row.get(5)?;
    let remote_ops_i64: i64 = row.get(6)?;
    let bytes_uploaded_i64: i64 = row.get(7)?;
    let bytes_downloaded_i64: i64 = row.get(8)?;
    let outcome_opt: Option<String> = row.get(9)?;
    let outcome_detail: Option<String> = row.get(10)?;

    Ok(SyncRun {
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
        trigger: run_trigger_from_str(&trigger_str)?,
        started_at: parse_timestamp(&started_at_str)?,
        finished_at: finished_at_opt.map(|s| parse_timestamp(&s)).transpose()?,
        local_ops: u32::try_from(local_ops_i64).unwrap_or(0),
        remote_ops: u32::try_from(remote_ops_i64).unwrap_or(0),
        bytes_uploaded: u64::try_from(bytes_uploaded_i64).unwrap_or(0),
        bytes_downloaded: u64::try_from(bytes_downloaded_i64).unwrap_or(0),
        outcome: outcome_opt.map(|s| run_outcome_from_str(&s)).transpose()?,
        outcome_detail,
    })
}

const fn run_trigger_to_str(t: RunTrigger) -> &'static str {
    match t {
        RunTrigger::Scheduled => "scheduled",
        RunTrigger::LocalEvent => "local_event",
        RunTrigger::RemoteWebhook => "remote_webhook",
        RunTrigger::CliForce => "cli_force",
        RunTrigger::BackoffRetry => "backoff_retry",
    }
}

fn run_trigger_from_str(s: &str) -> rusqlite::Result<RunTrigger> {
    match s {
        "scheduled" => Ok(RunTrigger::Scheduled),
        "local_event" => Ok(RunTrigger::LocalEvent),
        "remote_webhook" => Ok(RunTrigger::RemoteWebhook),
        "cli_force" => Ok(RunTrigger::CliForce),
        "backoff_retry" => Ok(RunTrigger::BackoffRetry),
        other => Err(rusqlite::Error::InvalidColumnType(
            2,
            format!("unknown run trigger: {other}"),
            rusqlite::types::Type::Text,
        )),
    }
}

const fn run_outcome_to_str(o: RunOutcome) -> &'static str {
    match o {
        RunOutcome::Success => "success",
        RunOutcome::PartialFailure => "partial_failure",
        RunOutcome::Aborted => "aborted",
    }
}

fn run_outcome_from_str(s: &str) -> rusqlite::Result<RunOutcome> {
    match s {
        "success" => Ok(RunOutcome::Success),
        "partial_failure" => Ok(RunOutcome::PartialFailure),
        "aborted" => Ok(RunOutcome::Aborted),
        other => Err(rusqlite::Error::InvalidColumnType(
            9,
            format!("unknown run outcome: {other}"),
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
    use std::time::Duration;
    use tempfile::TempDir;
    use ulid::Ulid;

    fn fresh_db() -> (TempDir, crate::connection::ConnectionPool) {
        let tmp = TempDir::new().expect("tmpdir");
        let pool = open(&tmp.path().join("t.sqlite")).expect("open");
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

    fn sample_run(pair: &Pair, seed: u128, started: Timestamp) -> SyncRun {
        SyncRun {
            id: Id::<SyncRunTag>::from_ulid(Ulid::from(seed)),
            pair_id: pair.id,
            trigger: RunTrigger::Scheduled,
            started_at: started,
            finished_at: None,
            local_ops: 0,
            remote_ops: 0,
            bytes_uploaded: 0,
            bytes_downloaded: 0,
            outcome: None,
            outcome_detail: None,
        }
    }

    #[test]
    fn record_then_recent_returns_the_run() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        let run = sample_run(&pair, 100u128 << 64, base_ts());
        record(&conn, &run).expect("record");

        let results = recent(&conn, &pair.id, 10).expect("recent");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, run.id);
        assert_eq!(results[0].trigger, RunTrigger::Scheduled);
    }

    #[test]
    fn record_is_idempotent() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        let run = sample_run(&pair, 101u128 << 64, base_ts());
        record(&conn, &run).expect("first record");

        // Update the run with finish information
        let finished_run = SyncRun {
            finished_at: Some(Timestamp::from_datetime(
                Utc.with_ymd_and_hms(2026, 5, 12, 11, 0, 0).unwrap(),
            )),
            local_ops: 5,
            remote_ops: 3,
            bytes_uploaded: 1024,
            bytes_downloaded: 512,
            outcome: Some(RunOutcome::Success),
            outcome_detail: Some("all done".to_owned()),
            ..run
        };
        record(&conn, &finished_run).expect("second record (upsert)");

        let results = recent(&conn, &pair.id, 10).expect("recent");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].finished_at, finished_run.finished_at);
        assert_eq!(results[0].local_ops, 5);
        assert_eq!(results[0].outcome, Some(RunOutcome::Success));
    }

    #[test]
    fn recent_orders_by_started_at_desc() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        let clock = TestClock::at(Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap());

        let t1 = clock.now();
        clock.advance(Duration::from_mins(1));
        let t2 = clock.now();
        clock.advance(Duration::from_mins(1));
        let t3 = clock.now();

        let run1 = sample_run(&pair, 201u128 << 64, t1);
        let run2 = sample_run(&pair, 202u128 << 64, t2);
        let run3 = sample_run(&pair, 203u128 << 64, t3);

        record(&conn, &run1).expect("record 1");
        record(&conn, &run2).expect("record 2");
        record(&conn, &run3).expect("record 3");

        let results = recent(&conn, &pair.id, 10).expect("recent");
        assert_eq!(results.len(), 3);
        // Most recent first
        assert_eq!(results[0].id, run3.id);
        assert_eq!(results[1].id, run2.id);
        assert_eq!(results[2].id, run1.id);
    }

    #[test]
    fn recent_respects_limit() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let (_, pair) = insert_prerequisites(&conn);

        let clock = TestClock::at(Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap());

        for seed in 0..5u128 {
            let ts = clock.now();
            clock.advance(Duration::from_secs(1));
            let run = sample_run(&pair, (seed + 300) << 64, ts);
            record(&conn, &run).expect("record");
        }

        let results = recent(&conn, &pair.id, 3).expect("recent");
        assert_eq!(results.len(), 3);
    }
}
