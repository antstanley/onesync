//! Integration test: dirty cycle on the local side produces an Upload op.
//!
//! Pre-seed: a `FileEntry` for the pair with `sync_state = Dirty`, `local`
//! populated, `synced` absent. The engine's incremental local-scan path pulls
//! dirty entries from the state store; reconcile sees a local-only change and
//! plans an Upload; the executor drives it via the fake `RemoteDrive`.
//!
//! The remote-modified-download path is NOT exercised here — it requires
//! populated `DeltaPage` items, which the current port shape doesn't expose.
//! That coverage lands in a follow-up when the daemon-level scheduler is
//! wired up.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod helpers;

use helpers::FakeContext;
use onesync_core::engine::cycle::run_cycle;
use onesync_core::ports::{Clock, StateStore};
use onesync_protocol::{
    enums::{FileKind, FileSyncState, RunOutcome, RunTrigger},
    file_entry::FileEntry,
    file_side::FileSide,
    path::{AbsPath, RelPath},
    primitives::{ContentHash, Timestamp},
};

fn rel(s: &str) -> RelPath {
    s.parse().expect("rel path")
}

const fn side_at(seed: u8, mtime: Timestamp) -> FileSide {
    FileSide {
        kind: FileKind::File,
        size_bytes: 13,
        content_hash: Some(ContentHash::from_bytes([seed; 32])),
        mtime,
        etag: None,
        remote_item_id: None,
    }
}

#[tokio::test]
async fn dirty_local_only_plans_an_upload_and_completes_successfully() {
    let ctx = FakeContext::new();
    let (pair_id, pair) = ctx.seed_pair().await;
    let now = ctx.clock.now();

    // Seed the file in the fake fs at the absolute path the executor will read.
    let abs_path: AbsPath = format!("{}/notes.md", pair.local_path.as_str())
        .parse()
        .expect("abs path");
    ctx.local.seed_file(&abs_path, b"hello onesync", now);

    let entry = FileEntry {
        pair_id,
        relative_path: rel("notes.md"),
        kind: FileKind::File,
        sync_state: FileSyncState::Dirty,
        local: Some(side_at(0x11, now)),
        remote: None,
        synced: None,
        pending_op_id: None,
        updated_at: now,
    };
    ctx.store
        .file_entry_upsert(&entry)
        .await
        .expect("upsert dirty entry");

    let summary = run_cycle(&ctx.deps(), pair_id, RunTrigger::Scheduled)
        .await
        .expect("dirty cycle should succeed");

    assert_eq!(summary.outcome, RunOutcome::Success);
    // The engine recorded at least one remote-side op (the upload).
    assert!(
        summary.remote_ops >= 1,
        "expected at least one remote op, got {}",
        summary.remote_ops
    );
}
