//! Regression tests for RP1-F7 (`docs/reviews/2026-05-15-sync-engine.md`):
//!
//! Spec lists eight `FileOpKind` variants; the executor previously dispatched
//! only four and returned `NotImplemented` for the rest, so any planner-emitted
//! `RemoteMkdir`/`RemoteDelete`/`LocalRename`/`RemoteRename` was marked Failed
//! at first attempt with no retry. These integration tests pin the
//! corrected behaviour: every kind reaches its port call.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    clippy::panic
)]

use onesync_core::engine::executor::execute;
use onesync_core::ports::LocalFsError;
use onesync_fs_local::fakes::InMemoryLocalFs;
use onesync_graph::fakes::FakeRemoteDrive;
use onesync_protocol::{
    enums::{FileOpKind, FileOpStatus},
    file_op::FileOp,
    id::{Id, IdPrefix},
    path::AbsPath,
    primitives::Timestamp,
};
use ulid::Ulid;

fn ts() -> Timestamp {
    Timestamp::from_datetime(
        chrono::DateTime::from_timestamp(0, 0).unwrap_or_else(chrono::Utc::now),
    )
}

fn id<P: IdPrefix + 'static>() -> Id<P> {
    Id::from_ulid(Ulid::new())
}

fn op(
    kind: FileOpKind,
    path: &str,
    metadata: serde_json::Map<String, serde_json::Value>,
) -> FileOp {
    FileOp {
        id: id(),
        run_id: id(),
        pair_id: id(),
        relative_path: path.parse().unwrap(),
        kind,
        status: FileOpStatus::Enqueued,
        attempts: 0,
        last_error: None,
        metadata,
        enqueued_at: ts(),
        started_at: None,
        finished_at: None,
    }
}

#[tokio::test]
async fn remote_mkdir_creates_folder() {
    let remote = FakeRemoteDrive::new();
    let root = remote.mkdir_sync("", "root");
    let local = InMemoryLocalFs::new();
    let local_root: AbsPath = "/tmp/rp1-f7".parse().unwrap();

    let mut meta = serde_json::Map::new();
    meta.insert(
        "parent_remote_id".to_owned(),
        serde_json::Value::String(root.id.clone()),
    );
    let op = op(FileOpKind::RemoteMkdir, "newdir", meta);
    let status = execute(&op, &local_root, &local, &remote).await.unwrap();
    assert_eq!(status, FileOpStatus::Success);
}

#[tokio::test]
async fn remote_delete_succeeds_for_existing_item() {
    let remote = FakeRemoteDrive::new();
    let root = remote.mkdir_sync("", "root");
    let item = remote.upload_sync(&root.id, "victim.txt", bytes::Bytes::from_static(b"x"));
    let local = InMemoryLocalFs::new();
    let local_root: AbsPath = "/tmp/rp1-f7".parse().unwrap();

    let mut meta = serde_json::Map::new();
    meta.insert(
        "remote_item_id".to_owned(),
        serde_json::Value::String(item.id.clone()),
    );
    let op = op(FileOpKind::RemoteDelete, "victim.txt", meta);
    let status = execute(&op, &local_root, &local, &remote).await.unwrap();
    assert_eq!(status, FileOpStatus::Success);
    // And the item is actually gone from the fake.
    assert!(remote.delete_sync(&item.id).is_err());
}

#[tokio::test]
async fn local_rename_moves_file() {
    let remote = FakeRemoteDrive::new();
    let local = InMemoryLocalFs::new();
    let local_root: AbsPath = "/tmp/rp1-f7".parse().unwrap();
    let from_abs: AbsPath = "/tmp/rp1-f7/a.txt".parse().unwrap();
    local.seed_file(&from_abs, b"hello", ts());

    let mut meta = serde_json::Map::new();
    meta.insert(
        "new_path".to_owned(),
        serde_json::Value::String("b.txt".to_owned()),
    );
    let op = op(FileOpKind::LocalRename, "a.txt", meta);
    let status = execute(&op, &local_root, &local, &remote).await.unwrap();
    assert_eq!(status, FileOpStatus::Success);
}

#[tokio::test]
async fn remote_rename_changes_name() {
    let remote = FakeRemoteDrive::new();
    let root = remote.mkdir_sync("", "root");
    let item = remote.upload_sync(&root.id, "old.txt", bytes::Bytes::from_static(b"x"));
    let local = InMemoryLocalFs::new();
    let local_root: AbsPath = "/tmp/rp1-f7".parse().unwrap();

    let mut meta = serde_json::Map::new();
    meta.insert(
        "remote_item_id".to_owned(),
        serde_json::Value::String(item.id.clone()),
    );
    meta.insert(
        "new_name".to_owned(),
        serde_json::Value::String("new.txt".to_owned()),
    );
    let op = op(FileOpKind::RemoteRename, "old.txt", meta);
    let status = execute(&op, &local_root, &local, &remote).await.unwrap();
    assert_eq!(status, FileOpStatus::Success);
}

/// Missing rename metadata is an invariant violation, not a port-level
/// transient — surface it as `Local(InvalidPath)` so the cycle marks the op
/// `Failed` cleanly rather than retrying.
#[tokio::test]
async fn local_rename_without_new_path_fails_with_invalid_path() {
    let remote = FakeRemoteDrive::new();
    let local = InMemoryLocalFs::new();
    let local_root: AbsPath = "/tmp/rp1-f7".parse().unwrap();
    let from_abs: AbsPath = "/tmp/rp1-f7/a.txt".parse().unwrap();
    local.seed_file(&from_abs, b"hello", ts());

    let op = op(FileOpKind::LocalRename, "a.txt", serde_json::Map::new());
    let err = execute(&op, &local_root, &local, &remote)
        .await
        .expect_err("must error, not silently succeed");
    let local_err = match err {
        onesync_core::engine::executor::ExecError::Local(e) => e,
        other => panic!("expected Local(InvalidPath), got {other:?}"),
    };
    assert!(
        matches!(local_err, LocalFsError::InvalidPath { .. }),
        "expected InvalidPath, got {local_err:?}"
    );
}
