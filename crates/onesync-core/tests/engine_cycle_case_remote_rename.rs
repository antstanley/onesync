//! Regression test for RP1-F24 (`docs/reviews/2026-05-15-sync-engine.md`):
//!
//! `state.file_entry_get` is byte-exact. APFS folds `Foo.txt` and `foo.txt`
//! to the same inode, but if the engine stored the `FileEntry` under one case
//! and the remote delta arrives with a different case (typical: a remote
//! rename), the exact-match lookup returns `None` and the engine treats it
//! as a brand-new item, downloads it, and APFS overwrites the original
//! local file's content.
//!
//! Post-fix: when the byte-exact lookup misses, the cycle does a
//! case-insensitive lookup. If a `FileEntry` exists under a different-case
//! `relative_path`, the cycle skips the delta item with an audit event
//! `file_entry.case_collision_detected` instead of upserting a parallel
//! `FileEntry` and emitting a Download decision.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use onesync_core::engine::{cycle::CycleCtx, run_cycle};
use onesync_core::ports::{Clock, StateStore};
use onesync_fs_local::fakes::InMemoryLocalFs;
use onesync_graph::fakes::FakeRemoteDrive;
use onesync_protocol::{
    enums::{FileKind, FileSyncState, RunTrigger},
    file_entry::FileEntry,
    file_side::FileSide,
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
async fn remote_rename_at_different_case_does_not_clobber_local_entry() {
    let pair_id = pair_id();
    let local_root: onesync_protocol::path::AbsPath = "/tmp/rp1-f24".parse().unwrap();
    let drive_id = DriveId::new("drive-test");

    let state = InMemoryStore::new();
    let remote = FakeRemoteDrive::new();
    let local_fs = InMemoryLocalFs::new();
    let clock = EpochClock;
    let ids = UlidGenerator::default();
    let audit = DevNullAudit;
    let now = clock.now();

    // Seed: existing FileEntry at "Foo.txt" with synced local content.
    let existing_side = FileSide {
        kind: FileKind::File,
        size_bytes: 6,
        content_hash: None,
        mtime: now,
        etag: None,
        remote_item_id: None,
    };
    let existing = FileEntry {
        pair_id,
        relative_path: "Foo.txt".parse().unwrap(),
        kind: FileKind::File,
        sync_state: FileSyncState::Clean,
        local: Some(existing_side.clone()),
        remote: None,
        synced: Some(existing_side),
        pending_op_id: None,
        updated_at: now,
    };
    state.file_entry_upsert(&existing).await.unwrap();

    // Remote delta returns the same logical file under a different case.
    let _root = remote.mkdir_sync("", "root");
    let _file = remote.upload_sync(
        "root",
        "foo.txt",
        bytes::Bytes::from_static(b"remote-content"),
    );

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
    let _ = run_cycle(&ctx).await.expect("cycle runs");

    // RP1-F24: no FileEntry should be synthesized at the new-case path.
    let new_case: onesync_protocol::path::RelPath = "foo.txt".parse().unwrap();
    let entry_new = state.file_entry_get(&pair_id, &new_case).await.unwrap();
    assert!(
        entry_new.is_none(),
        "no parallel FileEntry must be created at {new_case} when one already exists case-folded"
    );

    // The original FileEntry must survive intact (no overwrite of synced).
    let existing_case: onesync_protocol::path::RelPath = "Foo.txt".parse().unwrap();
    let entry_old = state
        .file_entry_get(&pair_id, &existing_case)
        .await
        .unwrap()
        .expect("original FileEntry must survive");
    assert_eq!(entry_old.sync_state, FileSyncState::Clean);
    assert!(entry_old.synced.is_some());

    // RP1-F24 follow-on: the colliding delta item must have been auto-
    // renamed on remote with a `(case-collision-XXXXXXX).<ext>` suffix.
    // The canonical local case is `Foo.txt`; the remote arrived as
    // `foo.txt`; the engine renames the remote item to disambiguate.
    let (items, _) = remote.delta_all_sync();
    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    assert!(
        !names.contains(&"foo.txt"),
        "remote `foo.txt` should have been renamed to a case-collision name, got names = {names:?}"
    );
    assert!(
        names
            .iter()
            .any(|n| n.contains("(case-collision-") && n.contains(".txt")),
        "expected a `(case-collision-XXXXXXX).txt` entry on remote, got names = {names:?}"
    );
}
