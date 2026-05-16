//! RP1-F14 follow-on (`docs/reviews/2026-05-15-sync-engine.md`):
//!
//! Detection of remote-side case-collisions was added in commit `0f7ef3c7`
//! — losers in a `(Foo.txt, foo.txt)` pair were dropped from reconcile and
//! audited. This test pins the *auto-rename* extension: the cycle now emits
//! a `RemoteRename` op for each loser using `case_collision_rename_target`,
//! so both files end up on remote with distinct names and a subsequent
//! cycle naturally syncs them both.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use onesync_core::engine::{cycle::CycleCtx, run_cycle};
use onesync_fs_local::fakes::InMemoryLocalFs;
use onesync_graph::fakes::FakeRemoteDrive;
use onesync_protocol::{
    enums::RunTrigger,
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
async fn remote_case_collision_losers_are_renamed_remotely() {
    let pair_id = pair_id();
    let local_root: onesync_protocol::path::AbsPath = "/tmp/rp1-f14-followon".parse().unwrap();
    let drive_id = DriveId::new("drive-test");

    let state = InMemoryStore::new();
    let remote = FakeRemoteDrive::new();
    let local_fs = InMemoryLocalFs::new();
    let clock = EpochClock;
    let ids = UlidGenerator::default();
    let audit = DevNullAudit;

    // Two remote items that ASCII-case-fold to the same path.
    let _root = remote.mkdir_sync("", "root");
    let upper = remote.upload_sync("root", "Foo.txt", bytes::Bytes::from_static(b"upper"));
    let lower = remote.upload_sync("root", "foo.txt", bytes::Bytes::from_static(b"lower"));

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

    // After the cycle, the loser must have a disambiguated name remotely.
    // `Foo.txt` is byte-wise smaller (uppercase F = 0x46 < lowercase f =
    // 0x66) so `foo.txt` is the loser.
    let (items, _) = remote.delta_all_sync();
    let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
    assert!(
        names.contains(&"Foo.txt"),
        "canonical Foo.txt must survive untouched, got names = {names:?}"
    );
    assert!(
        !names.contains(&"foo.txt"),
        "loser foo.txt must have been renamed remotely, got names = {names:?}"
    );
    assert!(
        names
            .iter()
            .any(|n| n.contains("(case-collision-") && n.contains(".txt")),
        "expected a `(case-collision-XXXXXXX).txt` entry in remote item names, got {names:?}"
    );

    // The canonical and the original loser item still have the same ids
    // (the rename mutates name, not id) — sanity check that we renamed the
    // loser specifically, not the canonical.
    let by_id: std::collections::HashMap<&str, &str> = items
        .iter()
        .map(|i| (i.id.as_str(), i.name.as_str()))
        .collect();
    assert_eq!(by_id.get(upper.id.as_str()), Some(&"Foo.txt"));
    assert!(
        by_id
            .get(lower.id.as_str())
            .is_some_and(|n| n.contains("(case-collision-")),
        "loser item id should now resolve to a renamed name, got by_id[{}] = {:?}",
        lower.id,
        by_id.get(lower.id.as_str())
    );
}
