//! Integration tests for `phase_local_uploads`: untracked or diverged local files
//! produce `Upload`/`RemoteMkdir` decisions and corresponding `PendingUpload` entries.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use onesync_core::engine::{cycle::CycleCtx, run_cycle};
use onesync_core::ports::{Clock, StateStore};
use onesync_fs_local::fakes::InMemoryLocalFs;
use onesync_graph::fakes::FakeRemoteDrive;
use onesync_protocol::{
    enums::{FileSyncState, RunTrigger},
    primitives::{DriveId, Timestamp},
};
use onesync_state::fakes::InMemoryStore;
use onesync_time::ulid_generator::UlidGenerator;
use ulid::Ulid;

fn pair_id() -> onesync_protocol::id::PairId {
    onesync_protocol::id::PairId::from_ulid(Ulid::new())
}

struct DevNullAudit;
impl onesync_core::ports::AuditSink for DevNullAudit {
    fn emit(&self, _event: onesync_protocol::audit::AuditEvent) {}
}

struct EpochClock;
impl onesync_core::ports::Clock for EpochClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_datetime(
            chrono::DateTime::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
        )
    }
}

#[tokio::test]
async fn untracked_local_file_inserts_pending_upload_entry() {
    let pair_id = pair_id();
    let local_root: onesync_protocol::path::AbsPath = "/tmp/m12b-upload".parse().unwrap();
    let drive_id = DriveId::new("drive-test");

    let state = InMemoryStore::new();
    let remote = FakeRemoteDrive::new();
    let local_fs = InMemoryLocalFs::new();
    let clock = EpochClock;
    let ids = UlidGenerator::default();
    let audit = DevNullAudit;

    let file_abs: onesync_protocol::path::AbsPath = "/tmp/m12b-upload/notes.md".parse().unwrap();
    local_fs.seed_file(&file_abs, b"hello", clock.now());

    let ctx = CycleCtx {
        pair_id,
        local_root,
        drive_id,
        cursor: None,
        trigger: RunTrigger::Scheduled,
        state: &state,
        remote: &remote,
        local: &local_fs,
        audit: &audit,
        clock: &clock,
        ids: &ids,
        host_name: "testhost".to_owned(),
    };

    let summary = run_cycle(&ctx).await.expect("cycle runs without error");

    assert_eq!(summary.remote_items_seen, 0);
    assert_eq!(summary.local_events_seen, 1, "scan should see 1 local path");

    let rel: onesync_protocol::path::RelPath = "notes.md".parse().unwrap();
    let entry = state
        .file_entry_get(&pair_id, &rel)
        .await
        .unwrap()
        .expect("entry was upserted");
    assert_eq!(entry.sync_state, FileSyncState::PendingUpload);
    assert!(entry.local.is_some());
    assert!(entry.synced.is_none());
}

#[tokio::test]
async fn remote_delta_paths_are_not_double_uploaded() {
    let pair_id = pair_id();
    let local_root: onesync_protocol::path::AbsPath = "/tmp/m12b-dedup".parse().unwrap();
    let drive_id = DriveId::new("drive-test");

    let state = InMemoryStore::new();
    let remote = FakeRemoteDrive::new();
    // Seed a remote file at the same path so phase_delta_reconcile claims it first.
    let _root = remote.mkdir_sync("", "root");
    let _file = remote.upload_sync("root", "shared.txt", bytes::Bytes::from_static(b"r"));

    let local_fs = InMemoryLocalFs::new();
    let clock = EpochClock;
    let ids = UlidGenerator::default();
    let audit = DevNullAudit;

    // Local file at the same relative path: should be skipped by phase_local_uploads
    // because phase_delta_reconcile already produced a Download decision for it.
    let file_abs: onesync_protocol::path::AbsPath = "/tmp/m12b-dedup/shared.txt".parse().unwrap();
    local_fs.seed_file(&file_abs, b"l", clock.now());

    let ctx = CycleCtx {
        pair_id,
        local_root,
        drive_id,
        cursor: None,
        trigger: RunTrigger::Scheduled,
        state: &state,
        remote: &remote,
        local: &local_fs,
        audit: &audit,
        clock: &clock,
        ids: &ids,
        host_name: "testhost".to_owned(),
    };

    let summary = run_cycle(&ctx).await.expect("cycle runs without error");

    // The local scan still considers 1 path (the file) but dedupes against the
    // remote-decision set, so no PendingUpload entry is created.
    let rel: onesync_protocol::path::RelPath = "shared.txt".parse().unwrap();
    let entry = state.file_entry_get(&pair_id, &rel).await.unwrap();
    assert!(
        entry.is_none() || entry.unwrap().sync_state != FileSyncState::PendingUpload,
        "remote-driven path must not also produce a PendingUpload entry"
    );
    assert!(
        summary.remote_items_seen >= 1,
        "remote scan should observe the shared file"
    );
}

#[tokio::test]
async fn local_file_matching_synced_snapshot_is_skipped() {
    let pair_id = pair_id();
    let local_root: onesync_protocol::path::AbsPath = "/tmp/m12b-clean".parse().unwrap();
    let drive_id = DriveId::new("drive-test");

    let state = InMemoryStore::new();
    let remote = FakeRemoteDrive::new();
    let local_fs = InMemoryLocalFs::new();
    let clock = EpochClock;
    let ids = UlidGenerator::default();
    let audit = DevNullAudit;

    let file_abs: onesync_protocol::path::AbsPath = "/tmp/m12b-clean/stable.txt".parse().unwrap();
    let mtime = clock.now();
    let body = b"stable";
    local_fs.seed_file(&file_abs, body, mtime);

    // Seed a synced snapshot that matches the on-disk file.
    let rel: onesync_protocol::path::RelPath = "stable.txt".parse().unwrap();
    let synced = onesync_protocol::file_side::FileSide {
        kind: onesync_protocol::enums::FileKind::File,
        size_bytes: body.len() as u64,
        content_hash: None,
        mtime,
        etag: None,
        remote_item_id: None,
    };
    let entry = onesync_protocol::file_entry::FileEntry {
        pair_id,
        relative_path: rel.clone(),
        kind: onesync_protocol::enums::FileKind::File,
        sync_state: FileSyncState::Clean,
        local: Some(synced.clone()),
        remote: None,
        synced: Some(synced),
        pending_op_id: None,
        updated_at: mtime,
    };
    state.file_entry_upsert(&entry).await.unwrap();

    let ctx = CycleCtx {
        pair_id,
        local_root,
        drive_id,
        cursor: None,
        trigger: RunTrigger::Scheduled,
        state: &state,
        remote: &remote,
        local: &local_fs,
        audit: &audit,
        clock: &clock,
        ids: &ids,
        host_name: "testhost".to_owned(),
    };

    run_cycle(&ctx).await.expect("cycle runs without error");

    let after = state.file_entry_get(&pair_id, &rel).await.unwrap().unwrap();
    assert_eq!(
        after.sync_state,
        FileSyncState::Clean,
        "unchanged file must not be flipped to PendingUpload"
    );
}
