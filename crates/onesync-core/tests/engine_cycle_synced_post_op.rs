//! Regression test for RP1-F11 (`docs/reviews/2026-05-15-sync-engine.md`):
//!
//! Spec [`docs/spec/03-sync-engine.md`] line 230-231: on op success the engine
//! "updates `FileEntry.synced` to match the post-op state and transitions to
//! `Success`." The pre-fix cycle only flipped `FileOp.status`; `FileEntry`
//! kept its stale `synced` snapshot, so the next reconcile saw the same
//! divergence and re-emitted the op indefinitely.

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

/// Upload happy path: after the cycle, the upserted entry's `synced` reflects
/// the local side and `sync_state` transitions to `Clean`.
#[tokio::test]
async fn upload_op_success_updates_synced_to_local_and_clears_state() {
    let pair_id = pair_id();
    let local_root: onesync_protocol::path::AbsPath = "/tmp/rp1-f11-upload".parse().unwrap();
    let drive_id = DriveId::new("drive-test");

    let state = InMemoryStore::new();
    let remote = FakeRemoteDrive::new();
    let local_fs = InMemoryLocalFs::new();
    let clock = EpochClock;
    let ids = UlidGenerator::default();
    let audit = DevNullAudit;

    let file_abs: onesync_protocol::path::AbsPath = "/tmp/rp1-f11-upload/notes.md".parse().unwrap();
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
    let _ = run_cycle(&ctx).await.expect("cycle runs without error");

    let rel: onesync_protocol::path::RelPath = "notes.md".parse().unwrap();
    let entry = state
        .file_entry_get(&pair_id, &rel)
        .await
        .unwrap()
        .expect("entry must exist");
    let local = entry.local.as_ref().expect("local side present");
    let synced = entry
        .synced
        .as_ref()
        .expect("FileEntry.synced must be populated after a successful Upload");
    assert_eq!(
        synced.size_bytes, local.size_bytes,
        "post-upload synced.size_bytes must equal local.size_bytes"
    );
    assert_eq!(
        synced.kind, local.kind,
        "post-upload synced.kind must equal local.kind"
    );
    assert_eq!(
        synced.mtime, local.mtime,
        "post-upload synced.mtime must equal local.mtime"
    );
    assert_eq!(
        entry.sync_state,
        FileSyncState::Clean,
        "post-upload entry must transition to Clean"
    );
    assert!(
        entry.pending_op_id.is_none(),
        "post-success entry must clear pending_op_id"
    );
}
