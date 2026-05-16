//! Regression test for RP1-F17 (`docs/reviews/2026-05-15-sync-engine.md`):
//!
//! Spec [`docs/spec/03-sync-engine.md`] "Initial sync" rule: when the same
//! path appears on both sides with differing content at first-time encounter
//! (no prior `FileEntry`), the engine routes the observation through the
//! conflict policy rather than silently downloading and overwriting local
//! data.
//!
//! Pre-fix: `(None, Some(remote))` unconditionally produced a `Download`
//! decision. The cycle would overwrite the local file's content, losing the
//! user's local edits.
//!
//! Post-fix: `phase_local_uploads` observes the local file, surfaces it as
//! an `initial_sync_collision`, and the cycle upgrades the Download to a
//! `ConflictDetected` decision. `phase_resolve_conflicts` then records the
//! Conflict row and parks the entry in `PendingConflict`.

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
async fn initial_sync_collision_records_conflict_and_parks_entry() {
    let pair_id = pair_id();
    let local_root: onesync_protocol::path::AbsPath = "/tmp/rp1-f17".parse().unwrap();
    let drive_id = DriveId::new("drive-test");

    let state = InMemoryStore::new();
    let remote = FakeRemoteDrive::new();
    let local_fs = InMemoryLocalFs::new();
    let clock = EpochClock;
    let ids = UlidGenerator::default();
    let audit = DevNullAudit;

    // Remote has hello.txt with "remote-version".
    let _root = remote.mkdir_sync("", "root");
    let _file = remote.upload_sync(
        "root",
        "hello.txt",
        bytes::Bytes::from_static(b"remote-version"),
    );

    // Local has the same path with different content. No FileEntry seeded —
    // this is a cold-start initial sync.
    let file_abs: onesync_protocol::path::AbsPath = "/tmp/rp1-f17/hello.txt".parse().unwrap();
    local_fs.seed_file(&file_abs, b"local-version", clock.now());

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
    let summary = run_cycle(&ctx).await.expect("cycle runs");

    // The cycle observed the collision and counted it.
    assert!(
        summary.conflicts_detected >= 1,
        "expected at least one conflict from the initial-sync collision; got {summary:?}"
    );

    // A persisted Conflict row exists at the colliding path.
    let unresolved = state.conflicts_unresolved(&pair_id).await.unwrap();
    let collision = unresolved
        .iter()
        .find(|c| c.relative_path.as_str() == "hello.txt")
        .expect("Conflict row for hello.txt must be persisted by phase_resolve_conflicts");
    assert!(collision.resolution.is_none());

    // The FileEntry parked in PendingConflict so the next cycle won't
    // re-emit the same decision.
    let rel: onesync_protocol::path::RelPath = "hello.txt".parse().unwrap();
    let entry = state
        .file_entry_get(&pair_id, &rel)
        .await
        .unwrap()
        .expect("FileEntry must persist");
    assert_eq!(entry.sync_state, FileSyncState::PendingConflict);
    assert!(
        entry.local.is_some(),
        "RP1-F17 must attach the local side observed by the scan"
    );
    assert!(entry.remote.is_some(), "remote side from delta must remain");
}
