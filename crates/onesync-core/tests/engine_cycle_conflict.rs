//! Integration test: a cycle that sees both a local-side change and a remote-side change
//! for the same path produces a Conflict decision.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use onesync_core::{
    engine::{cycle::CycleCtx, run_cycle},
    ports::StateStore,
};
use onesync_fs_local::fakes::InMemoryLocalFs;
use onesync_graph::fakes::FakeRemoteDrive;
use onesync_protocol::{
    enums::{FileKind, FileSyncState, RunTrigger},
    file_entry::FileEntry,
    file_side::FileSide,
    primitives::{ContentHash, DriveId, Timestamp},
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

#[allow(clippy::missing_const_for_fn)]
// LINT: not const because chrono::DateTime::from_timestamp isn't const-fn.
fn ts(secs: i64) -> Timestamp {
    Timestamp::from_datetime(chrono::DateTime::from_timestamp(secs, 0).unwrap())
}

const fn hash(b: u8) -> ContentHash {
    ContentHash::from_bytes([b; 32])
}

fn file_side(size: u64, h: ContentHash) -> FileSide {
    FileSide {
        kind: FileKind::File,
        size_bytes: size,
        content_hash: Some(h),
        mtime: ts(1),
        etag: None,
        remote_item_id: None,
    }
}

#[tokio::test]
async fn conflict_when_local_and_remote_both_diverge_from_synced() {
    let pair_id = pair_id();
    let local_root: onesync_protocol::path::AbsPath = "/tmp/test-pair-conflict".parse().unwrap();
    let drive_id = DriveId::new("drive-test");

    let state = InMemoryStore::new();
    let remote = FakeRemoteDrive::new();

    // Seed: remote has a new version of "hello.txt".
    let _ = remote.upload_sync(
        "",
        "hello.txt",
        bytes::Bytes::from_static(b"remote-version"),
    );

    // Seed the state with a "synced" snapshot that differs from BOTH current local and current
    // remote — engine should detect "both changed" → Conflict.
    let synced_snap = file_side(8, hash(0xAA));
    let local_snap = file_side(10, hash(0xBB));
    let remote_snap = file_side(12, hash(0xCC));
    let entry = FileEntry {
        pair_id,
        relative_path: "hello.txt".parse().unwrap(),
        kind: FileKind::File,
        sync_state: FileSyncState::Dirty,
        local: Some(local_snap),
        remote: Some(remote_snap),
        synced: Some(synced_snap),
        pending_op_id: None,
        updated_at: ts(1),
    };
    state.file_entry_upsert(&entry).await.unwrap();

    let local_fs = InMemoryLocalFs::new();
    let clock = EpochClock;
    let ids = UlidGenerator::default();
    let audit = DevNullAudit;

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

    assert_eq!(
        summary.conflicts_detected, 1,
        "expected exactly one Conflict decision, got summary={summary:?}"
    );
    assert_eq!(summary.remote_items_seen, 1);
}
