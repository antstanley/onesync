//! End-to-end smoke test of `SqliteStore` against a real tempfile-backed db.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::default_trait_access
)] // LINT: integration test boilerplate.

use chrono::{TimeZone, Utc};
use onesync_core::ports::StateStore;
use onesync_protocol::{
    account::Account,
    audit::AuditEvent,
    conflict::Conflict,
    enums::{
        AccountKind, AuditLevel, ConflictSide, FileKind, FileOpKind, FileOpStatus, FileSyncState,
        PairStatus, RunOutcome, RunTrigger,
    },
    file_entry::FileEntry,
    file_op::FileOp,
    file_side::FileSide,
    id::{AccountTag, AuditTag, ConflictTag, FileOpTag, Id, PairTag, SyncRunTag},
    pair::Pair,
    path::{AbsPath, RelPath},
    primitives::{ContentHash, DriveId, DriveItemId, KeychainRef, Timestamp},
    sync_run::SyncRun,
};
use onesync_state::{SqliteStore, open};
use tempfile::TempDir;
use ulid::Ulid;

fn ts(seconds: i64) -> Timestamp {
    Timestamp::from_datetime(Utc.timestamp_opt(seconds, 0).unwrap())
}

fn id<T: onesync_protocol::id::IdPrefix>(n: u128) -> Id<T> {
    Id::from_ulid(Ulid::from(n))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_pipeline_round_trips() {
    let tmp = TempDir::new().expect("tmpdir");
    let pool = open(&tmp.path().join("t.sqlite"), &ts(1_700_000_000)).expect("open");
    let store = SqliteStore::new(pool);

    let acct = Account {
        id: id::<AccountTag>(1u128 << 64),
        kind: AccountKind::Personal,
        upn: "alice@example.com".into(),
        tenant_id: "tid".into(),
        drive_id: DriveId::new("drv"),
        display_name: "Alice".into(),
        keychain_ref: KeychainRef::new("kc"),
        scopes: vec!["Files.ReadWrite".into()],
        created_at: ts(1_700_000_000),
        updated_at: ts(1_700_000_000),
    };
    store.account_upsert(&acct).await.expect("acct upsert");
    let back = store
        .account_get(&acct.id)
        .await
        .expect("acct get")
        .expect("present");
    assert_eq!(back, acct);

    let pair = Pair {
        id: id::<PairTag>(2u128 << 64),
        account_id: acct.id,
        local_path: "/tmp/onedrive".parse::<AbsPath>().unwrap(),
        remote_item_id: DriveItemId::new("root"),
        remote_path: "/".into(),
        display_name: "OneDrive".into(),
        status: PairStatus::Active,
        paused: false,
        delta_token: None,
        errored_reason: None,
        created_at: ts(1_700_000_000),
        updated_at: ts(1_700_000_000),
        last_sync_at: None,
        conflict_count: 0,
        webhook_enabled: false,
    };
    store.pair_upsert(&pair).await.expect("pair upsert");
    let active = store.pairs_active().await.expect("active");
    assert_eq!(active.len(), 1);

    let entry = FileEntry {
        pair_id: pair.id,
        relative_path: "notes.md".parse::<RelPath>().unwrap(),
        kind: FileKind::File,
        sync_state: FileSyncState::Clean,
        local: None,
        remote: None,
        synced: None,
        pending_op_id: None,
        updated_at: ts(1_700_000_001),
    };
    store.file_entry_upsert(&entry).await.expect("entry upsert");

    let dirty = store.file_entries_dirty(&pair.id, 10).await.expect("dirty");
    assert!(dirty.is_empty(), "clean entries excluded");

    // Flip to dirty
    let mut dirty_entry = entry.clone();
    dirty_entry.sync_state = FileSyncState::Dirty;
    store
        .file_entry_upsert(&dirty_entry)
        .await
        .expect("entry upsert dirty");
    let dirty = store.file_entries_dirty(&pair.id, 10).await.expect("dirty");
    assert_eq!(dirty.len(), 1);

    let run = SyncRun {
        id: id::<SyncRunTag>(3u128 << 64),
        pair_id: pair.id,
        trigger: RunTrigger::Scheduled,
        started_at: ts(1_700_000_002),
        finished_at: Some(ts(1_700_000_003)),
        local_ops: 1,
        remote_ops: 0,
        bytes_uploaded: 100,
        bytes_downloaded: 0,
        outcome: Some(RunOutcome::Success),
        outcome_detail: None,
    };
    store.run_record(&run).await.expect("run record");

    let op = FileOp {
        id: id::<FileOpTag>(4u128 << 64),
        run_id: run.id,
        pair_id: pair.id,
        relative_path: "notes.md".parse::<RelPath>().unwrap(),
        kind: FileOpKind::Upload,
        status: FileOpStatus::Enqueued,
        attempts: 0,
        last_error: None,
        metadata: Default::default(),
        enqueued_at: ts(1_700_000_002),
        started_at: None,
        finished_at: None,
    };
    store.op_insert(&op).await.expect("op insert");
    store
        .op_update_status(&op.id, FileOpStatus::Success)
        .await
        .expect("op update");

    let conflict = Conflict {
        id: id::<ConflictTag>(5u128 << 64),
        pair_id: pair.id,
        relative_path: "notes.md".parse::<RelPath>().unwrap(),
        winner: ConflictSide::Local,
        loser_relative_path: "notes (conflict).md".parse::<RelPath>().unwrap(),
        local_side: FileSide {
            kind: FileKind::File,
            size_bytes: 10,
            content_hash: Some("00".repeat(32).parse::<ContentHash>().unwrap()),
            mtime: ts(1_700_000_001),
            etag: None,
            remote_item_id: None,
        },
        remote_side: FileSide {
            kind: FileKind::File,
            size_bytes: 10,
            content_hash: Some("ff".repeat(32).parse::<ContentHash>().unwrap()),
            mtime: ts(1_700_000_001),
            etag: None,
            remote_item_id: None,
        },
        detected_at: ts(1_700_000_004),
        resolved_at: None,
        resolution: None,
        note: None,
    };
    store
        .conflict_insert(&conflict)
        .await
        .expect("conflict insert");
    let unresolved = store
        .conflicts_unresolved(&pair.id)
        .await
        .expect("unresolved");
    assert_eq!(unresolved.len(), 1);

    let evt = AuditEvent {
        id: id::<AuditTag>(6u128 << 64),
        ts: ts(1_700_000_005),
        level: AuditLevel::Info,
        kind: "smoke.test".into(),
        pair_id: Some(pair.id),
        payload: Default::default(),
    };
    store.audit_append(&evt).await.expect("audit");
}
