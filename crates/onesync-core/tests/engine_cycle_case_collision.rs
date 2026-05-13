//! Integration test for scheduler-side case-collision detection (M12b Task F).
//!
//! When the remote drive holds `Report.pdf` and the local volume also contains
//! `report.pdf` (different case), the engine renames the local-side loser using
//! `case_collision_rename_target` and records a `Conflict` row. The remote winner is
//! left untouched.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use onesync_core::engine::{cycle::CycleCtx, run_cycle};
use onesync_core::ports::{Clock, StateStore};
use onesync_fs_local::fakes::InMemoryLocalFs;
use onesync_graph::fakes::FakeRemoteDrive;
use onesync_protocol::{
    enums::{ConflictSide, RunTrigger},
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
impl Clock for EpochClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_datetime(
            chrono::DateTime::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
        )
    }
}

#[tokio::test]
async fn case_collision_renames_local_loser_and_records_conflict() {
    let pair_id = pair_id();
    let local_root: onesync_protocol::path::AbsPath = "/tmp/m12b-case".parse().unwrap();
    let drive_id = DriveId::new("drive-test");

    let state = InMemoryStore::new();
    let remote = FakeRemoteDrive::new();
    let _root = remote.mkdir_sync("", "root");
    // Remote has `Report.pdf` (capital R).
    let _file = remote.upload_sync("root", "Report.pdf", bytes::Bytes::from_static(b"R"));

    let local_fs = InMemoryLocalFs::new();
    let clock = EpochClock;
    let ids = UlidGenerator::default();
    let audit = DevNullAudit;

    // Local has `report.pdf` (lowercase r) — different bytes, same case-fold.
    let collider_abs: onesync_protocol::path::AbsPath =
        "/tmp/m12b-case/report.pdf".parse().unwrap();
    local_fs.seed_file(&collider_abs, b"l", clock.now());

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
    assert!(
        summary.conflicts_detected >= 1,
        "expected at least one conflict, got {}",
        summary.conflicts_detected
    );

    // The conflict record exists, identifying remote `Report.pdf` as the winner and
    // a rename of the local loser under `(case-collision-...)`.
    let conflicts = state.conflicts_unresolved(&pair_id).await.unwrap();
    assert_eq!(conflicts.len(), 1, "expected exactly one Conflict row");
    let c = &conflicts[0];
    assert_eq!(c.relative_path.as_str(), "Report.pdf");
    assert!(
        c.loser_relative_path
            .as_str()
            .starts_with("report (case-collision-"),
        "loser_relative_path was {}",
        c.loser_relative_path.as_str()
    );
    assert!(c.loser_relative_path.as_str().ends_with(").pdf"));
    assert_eq!(c.winner, ConflictSide::Remote);
}
