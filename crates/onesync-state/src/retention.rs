//! Compaction and pruning job.

use rusqlite::{Connection, params};

use onesync_core::limits::{
    AUDIT_RETENTION_DAYS, CONFLICT_RETENTION_DAYS, RUN_HISTORY_RETENTION_DAYS,
};
use onesync_protocol::primitives::Timestamp;

use crate::error::StateStoreError;

const PAIR_REMOVED_RETENTION_DAYS: i64 = 7;

/// How many rows were pruned per table during a retention pass.
#[derive(Debug, Default, Clone)]
pub struct RetentionReport {
    /// Number of audit event rows deleted.
    pub audit_events: u64,
    /// Number of sync run rows deleted.
    pub sync_runs: u64,
    /// Number of conflict rows deleted.
    pub conflicts: u64,
    /// Number of pair rows deleted.
    pub pairs: u64,
}

/// Prune rows past the retention window. Idempotent.
///
/// After pruning, runs `PRAGMA optimize`. Does NOT run `VACUUM` (too disruptive).
///
/// # Errors
/// Returns the underlying `SQLite` error if any DELETE or PRAGMA fails.
pub fn run(conn: &Connection, now: &Timestamp) -> Result<RetentionReport, StateStoreError> {
    let now_dt = now.into_inner();
    let audit_cutoff =
        (now_dt - chrono::Duration::days(i64::from(AUDIT_RETENTION_DAYS))).to_rfc3339();
    let run_cutoff =
        (now_dt - chrono::Duration::days(i64::from(RUN_HISTORY_RETENTION_DAYS))).to_rfc3339();
    let conflict_cutoff =
        (now_dt - chrono::Duration::days(i64::from(CONFLICT_RETENTION_DAYS))).to_rfc3339();
    let pair_cutoff = (now_dt - chrono::Duration::days(PAIR_REMOVED_RETENTION_DAYS)).to_rfc3339();

    let map = |e: rusqlite::Error| StateStoreError::Sqlite(e.to_string());
    let audit_events = u64::try_from(
        conn.execute(
            "DELETE FROM audit_events WHERE ts < ?",
            params![audit_cutoff],
        )
        .map_err(map)?,
    )
    .unwrap_or(0);

    let sync_runs = u64::try_from(
        conn.execute(
            "DELETE FROM sync_runs WHERE started_at < ?",
            params![run_cutoff],
        )
        .map_err(map)?,
    )
    .unwrap_or(0);

    let conflicts = u64::try_from(
        conn.execute(
            "DELETE FROM conflicts WHERE resolved_at IS NOT NULL AND resolved_at < ?",
            params![conflict_cutoff],
        )
        .map_err(map)?,
    )
    .unwrap_or(0);

    let pairs = u64::try_from(
        conn.execute(
            "DELETE FROM pairs WHERE status = 'removed' AND updated_at < ?",
            params![pair_cutoff],
        )
        .map_err(map)?,
    )
    .unwrap_or(0);

    // PRAGMA optimize is cheap and safe.
    conn.execute("PRAGMA optimize", []).map_err(map)?;

    Ok(RetentionReport {
        audit_events,
        sync_runs,
        conflicts,
        pairs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open;
    use crate::queries::{accounts, audit, conflicts, file_entries, pairs, sync_runs};
    use chrono::{TimeZone, Utc};
    use onesync_protocol::{
        account::Account,
        audit::AuditEvent,
        conflict::Conflict,
        enums::{
            AccountKind, AuditLevel, ConflictSide, FileKind, FileSyncState, PairStatus, RunTrigger,
        },
        file_entry::FileEntry,
        file_side::FileSide,
        id::{AccountTag, AuditTag, ConflictTag, Id, PairTag, SyncRunTag},
        pair::Pair,
        primitives::{ContentHash, DriveId, DriveItemId, KeychainRef, Timestamp},
        sync_run::SyncRun,
    };
    use rusqlite::Connection as RawConn;
    use tempfile::TempDir;
    use ulid::Ulid;

    // Base reference: 2026-05-12 10:00:00 UTC ("today" in tests)
    fn base_ts() -> Timestamp {
        Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap())
    }

    fn ts_at(days_offset: i64) -> Timestamp {
        let base = Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap();
        Timestamp::from_datetime(base + chrono::Duration::days(days_offset))
    }

    fn fresh_db() -> (TempDir, crate::connection::ConnectionPool) {
        let tmp = TempDir::new().expect("tmpdir");
        let pool = open(
            &tmp.path().join("t.sqlite"),
            &crate::queries::test_timestamp(),
        )
        .expect("open");
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
            created_at: base_ts(),
            updated_at: base_ts(),
        }
    }

    fn sample_pair(account: &Account, seed: u128, local_path: &str, status: PairStatus) -> Pair {
        Pair {
            id: Id::<PairTag>::from_ulid(Ulid::from(seed)),
            account_id: account.id,
            local_path: local_path.parse().expect("abs path"),
            remote_item_id: DriveItemId::new(format!("item-{seed}")),
            remote_path: "/".into(),
            display_name: "OneDrive".into(),
            status,
            paused: false,
            delta_token: None,
            errored_reason: None,
            created_at: base_ts(),
            updated_at: base_ts(),
            last_sync_at: None,
            conflict_count: 0,
            webhook_enabled: false,
        }
    }

    fn make_audit_event(seed: u128, ts: Timestamp) -> AuditEvent {
        AuditEvent {
            id: Id::<AuditTag>::from_ulid(Ulid::from(seed)),
            ts,
            level: AuditLevel::Info,
            kind: "test_event".to_owned(),
            pair_id: None,
            payload: serde_json::Map::new(),
        }
    }

    fn make_sync_run(pair: &Pair, seed: u128, started_at: Timestamp) -> SyncRun {
        SyncRun {
            id: Id::<SyncRunTag>::from_ulid(Ulid::from(seed)),
            pair_id: pair.id,
            trigger: RunTrigger::Scheduled,
            started_at,
            finished_at: None,
            local_ops: 0,
            remote_ops: 0,
            bytes_uploaded: 0,
            bytes_downloaded: 0,
            outcome: None,
            outcome_detail: None,
        }
    }

    fn make_conflict(pair: &Pair, seed: u128, resolved_at: Option<Timestamp>) -> Conflict {
        let side = FileSide {
            kind: FileKind::File,
            size_bytes: 512,
            content_hash: Some(ContentHash::from_bytes([0xAA; 32])),
            mtime: base_ts(),
            etag: None,
            remote_item_id: None,
        };
        Conflict {
            id: Id::<ConflictTag>::from_ulid(Ulid::from(seed)),
            pair_id: pair.id,
            relative_path: "docs/file.txt".parse().expect("rel path"),
            winner: ConflictSide::Local,
            loser_relative_path: "docs/file (conflict).txt".parse().expect("rel path"),
            local_side: side.clone(),
            remote_side: side,
            detected_at: base_ts(),
            resolved_at,
            resolution: resolved_at.map(|_| onesync_protocol::enums::ConflictResolution::Auto),
            note: None,
        }
    }

    fn make_file_entry(pair: &Pair) -> FileEntry {
        FileEntry {
            pair_id: pair.id,
            relative_path: "hello.txt".parse().expect("rel path"),
            kind: FileKind::File,
            sync_state: FileSyncState::Clean,
            local: None,
            remote: None,
            synced: None,
            pending_op_id: None,
            updated_at: base_ts(),
        }
    }

    // ── Test 1: retention prunes old audit events ─────────────────────────────

    #[test]
    fn retention_prunes_old_audit_events() {
        // AUDIT_RETENTION_DAYS = 30. "now" = day 0.
        // Cutoff = day 0 - 30 days = day -30.
        // Events:
        //   day -45 → before cutoff → should be deleted
        //   day -29 → after cutoff  → should stay
        //   day   0 → today         → should stay
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");

        audit::append(&conn, &make_audit_event(1u128 << 64, ts_at(-45))).expect("e1");
        audit::append(&conn, &make_audit_event(2u128 << 64, ts_at(-29))).expect("e2");
        audit::append(&conn, &make_audit_event(3u128 << 64, ts_at(0))).expect("e3");

        let now = base_ts(); // day 0
        let report = run(&conn, &now).expect("retention");

        assert_eq!(
            report.audit_events, 1,
            "only the -45 day event should be pruned"
        );
        assert_eq!(report.sync_runs, 0);
        assert_eq!(report.conflicts, 0);
        assert_eq!(report.pairs, 0);

        // Verify two events survive: ts > cutoff
        let remaining = audit::recent(&conn, 10).expect("recent");
        assert_eq!(remaining.len(), 2, "day -29 and day 0 events must survive");
    }

    // ── Test 2: retention is idempotent ──────────────────────────────────────

    #[test]
    fn retention_is_idempotent() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");

        // Insert one old event (-45 days) and one recent one (today)
        audit::append(&conn, &make_audit_event(10u128 << 64, ts_at(-45))).expect("old");
        audit::append(&conn, &make_audit_event(11u128 << 64, ts_at(0))).expect("new");

        let now = base_ts();
        let first = run(&conn, &now).expect("first retention");
        assert_eq!(first.audit_events, 1, "first pass should prune 1");

        let second = run(&conn, &now).expect("second retention");
        assert_eq!(
            second.audit_events, 0,
            "second pass should prune 0 (idempotent)"
        );
        assert_eq!(second.sync_runs, 0);
        assert_eq!(second.conflicts, 0);
        assert_eq!(second.pairs, 0);
    }

    // ── Test 3: retention cascades on pair removal ────────────────────────────

    #[test]
    fn retention_cascades_pair_removal() {
        // Pair status='removed', updated_at = -8 days.
        // PAIR_REMOVED_RETENTION_DAYS = 7, so cutoff = today - 7 days = -7 days.
        // -8 < -7 → pair should be deleted, and its file_entry should cascade-delete.
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");

        let acct = sample_account();
        accounts::upsert(&conn, &acct).expect("account");

        // Pair that is removed and old enough to be pruned
        let mut pair = sample_pair(
            &acct,
            20u128 << 64,
            "/Users/alice/Work",
            PairStatus::Removed,
        );
        pair.updated_at = ts_at(-8);
        pairs::upsert(&conn, &pair).expect("pair upsert");

        // Insert a file_entry referencing this pair
        let entry = make_file_entry(&pair);
        file_entries::upsert(&conn, &entry).expect("file_entry");

        // Verify file_entry exists before retention
        let before = file_entries::get(&conn, &pair.id, &entry.relative_path).expect("get before");
        assert!(before.is_some(), "file_entry should exist before retention");

        let now = base_ts();
        let report = run(&conn, &now).expect("retention");

        assert_eq!(report.pairs, 1, "the removed pair should be pruned");

        // Verify pair is gone
        let pair_after = pairs::get(&conn, &pair.id).expect("pair get");
        assert!(pair_after.is_none(), "pair should be deleted");

        // Verify file_entry cascaded
        let entry_after =
            file_entries::get(&conn, &pair.id, &entry.relative_path).expect("get after");
        assert!(
            entry_after.is_none(),
            "file_entry should be cascade-deleted"
        );
    }

    // ── Test 4: sync_run retention boundary ──────────────────────────────────

    #[test]
    fn retention_prunes_old_sync_runs() {
        // RUN_HISTORY_RETENTION_DAYS = 90.
        // Runs at: -91 days (should be deleted) and -89 days (should stay).
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");

        let acct = sample_account();
        accounts::upsert(&conn, &acct).expect("account");
        let pair = sample_pair(&acct, 30u128 << 64, "/Users/alice/Docs", PairStatus::Active);
        pairs::upsert(&conn, &pair).expect("pair");

        let old_run = make_sync_run(&pair, 40u128 << 64, ts_at(-91));
        let recent_run = make_sync_run(&pair, 41u128 << 64, ts_at(-89));
        sync_runs::record(&conn, &old_run).expect("old run");
        sync_runs::record(&conn, &recent_run).expect("recent run");

        let now = base_ts();
        let report = run(&conn, &now).expect("retention");

        assert_eq!(report.sync_runs, 1, "only the -91 day run should be pruned");

        // The -89 day run should survive
        let remaining = sync_runs::recent(&conn, &pair.id, 10).expect("recent");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, recent_run.id);
    }

    // ── Test 5: conflict retention prunes resolved-and-old conflicts ──────────

    #[test]
    fn retention_prunes_old_resolved_conflicts() {
        // CONFLICT_RETENTION_DAYS = 180.
        // Resolved conflict at -181 days → pruned.
        // Resolved conflict at -179 days → stays.
        // Unresolved conflict → always stays regardless of age.
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");

        let acct = sample_account();
        accounts::upsert(&conn, &acct).expect("account");
        let pair = sample_pair(
            &acct,
            50u128 << 64,
            "/Users/alice/Photos",
            PairStatus::Active,
        );
        pairs::upsert(&conn, &pair).expect("pair");

        // Resolved conflict, old enough to be pruned
        let old_conflict = {
            let mut c = make_conflict(&pair, 60u128 << 64, Some(ts_at(-181)));
            c.relative_path = "old.txt".parse().expect("rel path");
            c.loser_relative_path = "old (conflict).txt".parse().expect("rel path");
            c
        };
        // Resolved conflict, within retention window
        let recent_conflict = {
            let mut c = make_conflict(&pair, 61u128 << 64, Some(ts_at(-179)));
            c.relative_path = "recent.txt".parse().expect("rel path");
            c.loser_relative_path = "recent (conflict).txt".parse().expect("rel path");
            c
        };
        // Unresolved conflict (should never be pruned by resolved_at filter)
        let unresolved_conflict = {
            let mut c = make_conflict(&pair, 62u128 << 64, None);
            c.relative_path = "unresolved.txt".parse().expect("rel path");
            c.loser_relative_path = "unresolved (conflict).txt".parse().expect("rel path");
            c
        };

        conflicts::insert(&conn, &old_conflict).expect("old conflict");
        conflicts::insert(&conn, &recent_conflict).expect("recent conflict");
        conflicts::insert(&conn, &unresolved_conflict).expect("unresolved conflict");

        let now = base_ts();
        let report = run(&conn, &now).expect("retention");

        assert_eq!(
            report.conflicts, 1,
            "only the -181 day conflict should be pruned"
        );

        // Verify unresolved conflict survives
        let remaining = conflicts::unresolved(&conn, &pair.id).expect("unresolved");
        assert_eq!(remaining.len(), 1, "unresolved conflict must survive");
        assert_eq!(remaining[0].id, unresolved_conflict.id);
    }

    // ── Test 6: pair boundary — pair updated exactly at cutoff survives ───────

    #[test]
    fn retention_pair_at_cutoff_survives() {
        // PAIR_REMOVED_RETENTION_DAYS = 7.
        // Pair at exactly -7 days: cutoff = today - 7 days.
        // DELETE WHERE updated_at < cutoff, so -7 == cutoff should NOT be deleted.
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");

        let acct = sample_account();
        accounts::upsert(&conn, &acct).expect("account");

        let mut pair = sample_pair(
            &acct,
            70u128 << 64,
            "/Users/alice/Edge",
            PairStatus::Removed,
        );
        pair.updated_at = ts_at(-7); // exactly at cutoff
        pairs::upsert(&conn, &pair).expect("pair");

        let now = base_ts();
        let report = run(&conn, &now).expect("retention");

        assert_eq!(report.pairs, 0, "pair at exactly the cutoff should survive");

        let pair_after = pairs::get(&conn, &pair.id).expect("get");
        assert!(
            pair_after.is_some(),
            "pair at cutoff boundary should not be deleted"
        );
    }

    // ── Test 7: raw connection path (no pool) — used by xtask ────────────────

    #[test]
    fn retention_works_with_raw_connection() {
        // The xtask uses a plain rusqlite::Connection (not from a pool).
        // Verify retention::run accepts &Connection directly.
        let mut raw_conn = RawConn::open_in_memory().expect("open");
        raw_conn
            .pragma_update(None, "foreign_keys", "ON")
            .expect("fk");
        crate::migrations::run(&mut raw_conn).expect("migrate");

        // Insert an old audit event
        let conn = raw_conn;
        audit::append(&conn, &make_audit_event(80u128 << 64, ts_at(-45))).expect("old event");

        let report = run(&conn, &base_ts()).expect("retention");
        assert_eq!(report.audit_events, 1);
    }
}
