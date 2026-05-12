//! Integration test: a clean cycle with no remote changes produces a zero-ops summary.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use onesync_core::engine::{cycle::CycleCtx, run_cycle};
use onesync_fs_local::fakes::InMemoryLocalFs;
use onesync_graph::fakes::FakeRemoteDrive;
use onesync_protocol::{enums::RunTrigger, primitives::DriveId};
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
    fn now(&self) -> onesync_protocol::primitives::Timestamp {
        // 1970-01-01T00:00:00Z — always valid.
        onesync_protocol::primitives::Timestamp::from_datetime(
            chrono::DateTime::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
        )
    }
}

#[tokio::test]
async fn clean_cycle_no_remote_items_produces_zero_ops() {
    let pair_id = pair_id();
    let local_root: onesync_protocol::path::AbsPath = "/tmp/test-pair".parse().unwrap();
    let drive_id = DriveId::new("drive-test");

    let state = InMemoryStore::new();
    let remote = FakeRemoteDrive::new();
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

    assert_eq!(summary.remote_items_seen, 0);
    assert_eq!(summary.ops_applied, 0);
    assert_eq!(summary.conflicts_detected, 0);
}
