//! Integration test: a cycle that sees a new remote item with no local tracking
//! plans and executes a Download op.

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
        onesync_protocol::primitives::Timestamp::from_datetime(
            chrono::DateTime::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
        )
    }
}

#[tokio::test]
async fn remote_dirty_cycle_downloads_new_remote_file() {
    let pair_id = pair_id();
    let local_root: onesync_protocol::path::AbsPath = "/tmp/test-pair-dirty".parse().unwrap();
    let drive_id = DriveId::new("drive-test");

    let state = InMemoryStore::new();
    let remote = FakeRemoteDrive::new();

    // Seed the fake drive with one file; state has no tracking yet → engine downloads.
    let _root = remote.mkdir_sync("", "root");
    let _file_meta = remote.upload_sync("root", "hello.txt", bytes::Bytes::from_static(b"hi"));

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

    // Two remote items are visible (root folder + hello.txt). LocalMkdir + Download are both
    // emitted as decisions; both are applied. Conflicts must be zero.
    assert_eq!(summary.remote_items_seen, 2);
    assert!(
        summary.ops_applied >= 1,
        "expected at least the file Download op to apply, got {}",
        summary.ops_applied
    );
    assert_eq!(summary.conflicts_detected, 0);
}
