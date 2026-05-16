//! Regression test for RP1-F10 (`docs/reviews/2026-05-15-sync-engine.md`):
//!
//! Spec [`docs/spec/03-sync-engine.md`] (line 102-105) requires the cycle to
//! (1) persist each delta-page item to `FileEntry.remote` and (2) advance
//! `Pair.delta_token` only after that persistence completes.
//!
//! The pre-fix cycle does neither: the new cursor is returned in the
//! `CycleSummary` but never written back to the `Pair`, and the remote-side
//! observations never reach `FileEntry.remote` from the delta phase.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use onesync_core::engine::{cycle::CycleCtx, run_cycle};
use onesync_core::ports::StateStore;
use onesync_fs_local::fakes::InMemoryLocalFs;
use onesync_graph::fakes::FakeRemoteDrive;
use onesync_protocol::{
    enums::{PairStatus, RunTrigger},
    pair::Pair,
    primitives::{DriveId, DriveItemId, Timestamp},
};
use onesync_state::fakes::InMemoryStore;
use onesync_time::ulid_generator::UlidGenerator;
use ulid::Ulid;

fn pair_id() -> onesync_protocol::id::PairId {
    onesync_protocol::id::PairId::from_ulid(Ulid::new())
}
fn account_id() -> onesync_protocol::id::AccountId {
    onesync_protocol::id::AccountId::from_ulid(Ulid::new())
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
async fn cycle_persists_delta_cursor_and_remote_file_entries() {
    let pair_id = pair_id();
    let account_id = account_id();
    let local_root: onesync_protocol::path::AbsPath = "/tmp/test-cursor".parse().unwrap();
    let drive_id = DriveId::new("drive-test");

    let state = InMemoryStore::new();
    let remote = FakeRemoteDrive::new();
    let local_fs = InMemoryLocalFs::new();
    let clock = EpochClock;
    let ids = UlidGenerator::default();
    let audit = DevNullAudit;
    let now = onesync_core::ports::Clock::now(&clock);

    // Seed the Pair row with delta_token = None — engine must populate it.
    let pair = Pair {
        id: pair_id,
        account_id,
        local_path: local_root.clone(),
        remote_item_id: DriveItemId::new("root"),
        remote_path: "/".into(),
        display_name: "TestPair".into(),
        status: PairStatus::Active,
        paused: false,
        delta_token: None,
        errored_reason: None,
        created_at: now,
        updated_at: now,
        last_sync_at: None,
        conflict_count: 0,
        webhook_enabled: false,
    };
    state.pair_upsert(&pair).await.unwrap();

    // Seed remote with one file at the root.
    let _root = remote.mkdir_sync("", "root");
    let _file = remote.upload_sync("root", "hello.txt", bytes::Bytes::from_static(b"hi"));

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

    // (A) Pair.delta_token must be persisted to match the summary.
    let after_pair = state
        .pair_get(&pair_id)
        .await
        .unwrap()
        .expect("Pair row must still exist");
    assert!(
        after_pair.delta_token.is_some(),
        "expected Pair.delta_token to be persisted after the cycle; \
         summary.delta_token = {:?}",
        summary.delta_token
    );
    assert_eq!(
        after_pair.delta_token, summary.delta_token,
        "Pair.delta_token must equal CycleSummary.delta_token after a successful cycle"
    );

    // (B) FileEntry.remote must be populated for each delta item before the cursor
    //     advance (spec 03-sync-engine.md line 103-105).
    let rel_path: onesync_protocol::path::RelPath = "hello.txt".parse().unwrap();
    let entry = state
        .file_entry_get(&pair_id, &rel_path)
        .await
        .unwrap()
        .expect("FileEntry for hello.txt must be persisted by the delta phase");
    assert!(
        entry.remote.is_some(),
        "FileEntry.remote must be populated by phase_delta_reconcile, \
         got remote=None for {rel_path}"
    );
}
