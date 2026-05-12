//! M2 acceptance: scan a temp directory, persist file entries via `SqliteStore`,
//! re-observe a content change, and confirm the dirty index reflects it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]
// LINT: integration test boilerplate.

use chrono::{TimeZone, Utc};
use onesync_core::ports::{LocalFs, LocalWriteStream, StateStore};
use onesync_fs_local::LocalFsAdapter;
use onesync_protocol::{
    account::Account,
    enums::{AccountKind, FileKind, FileSyncState, PairStatus},
    file_entry::FileEntry,
    file_side::FileSide,
    id::{AccountTag, Id, PairTag},
    pair::Pair,
    path::{AbsPath, RelPath},
    primitives::{ContentHash, DriveId, DriveItemId, KeychainRef, Timestamp},
};
use onesync_state::{SqliteStore, open};
use std::path::PathBuf;
use tempfile::TempDir;
use ulid::Ulid;

const TEST_FILES: usize = 12;

fn ts(seconds: i64) -> Timestamp {
    Timestamp::from_datetime(Utc.timestamp_opt(seconds, 0).unwrap())
}

fn abs(p: &std::path::Path) -> AbsPath {
    p.to_str().expect("utf8").parse().expect("absolutepath")
}

fn rel(s: &str) -> RelPath {
    s.parse().expect("relative")
}

fn id<T: onesync_protocol::id::IdPrefix>(n: u128) -> Id<T> {
    Id::from_ulid(Ulid::from(n))
}

#[allow(clippy::cast_possible_truncation)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn m2_scan_state_dirty_pipeline() {
    let work = TempDir::new().expect("tmpdir");
    let pair_root = work.path().join("pair");
    std::fs::create_dir(&pair_root).expect("mk pair root");

    // Step 1-2: open the state store.
    let db = work.path().join("state.sqlite");
    let pool = open(&db, &ts(1_700_000_000)).expect("open");
    let store = SqliteStore::new(pool);

    // Step 3-4: adapter + synthesise N files on disk.
    let fs = LocalFsAdapter;
    for i in 0..TEST_FILES {
        let p = pair_root.join(format!("f{i:03}.bin"));
        let bytes = format!("payload-{i}").into_bytes();
        std::fs::write(&p, &bytes).expect("write fixture");
    }

    // Step 5: scan + hash via adapter.
    let scan = fs.scan(&abs(&pair_root)).await.expect("scan");
    assert_eq!(scan.0.len(), TEST_FILES, "scan should report every fixture");

    let mut observed: Vec<(PathBuf, ContentHash)> = Vec::new();
    for (path, side) in &scan.0 {
        if side.kind == FileKind::File {
            let h = fs.hash(&abs(path)).await.expect("hash");
            observed.push((path.clone(), h));
        }
    }
    assert_eq!(observed.len(), TEST_FILES);

    // Step 6: upsert account + pair, then a FileEntry per file (all Clean).
    let acct = Account {
        id: id::<AccountTag>(1u128 << 64),
        kind: AccountKind::Personal,
        upn: "alice@example.com".into(),
        tenant_id: "tid".into(),
        drive_id: DriveId::new("drv"),
        display_name: "Alice".into(),
        keychain_ref: KeychainRef::new("kc"),
        scopes: vec!["Files.ReadWrite".into()],
        created_at: ts(1_700_000_000),
        updated_at: ts(1_700_000_000),
    };
    store.account_upsert(&acct).await.expect("acct upsert");

    let pair = Pair {
        id: id::<PairTag>(2u128 << 64),
        account_id: acct.id,
        local_path: abs(&pair_root),
        remote_item_id: DriveItemId::new("root"),
        remote_path: "/".into(),
        display_name: "OneDrive".into(),
        status: PairStatus::Active,
        paused: false,
        delta_token: None,
        errored_reason: None,
        created_at: ts(1_700_000_000),
        updated_at: ts(1_700_000_000),
        last_sync_at: None,
        conflict_count: 0,
        webhook_enabled: false,
    };
    store.pair_upsert(&pair).await.expect("pair upsert");

    for (i, (path, hash)) in observed.iter().enumerate() {
        let rel_path = rel(&format!("f{i:03}.bin"));
        let bytes_len = std::fs::metadata(path).expect("meta").len();
        let side = FileSide {
            kind: FileKind::File,
            size_bytes: bytes_len,
            content_hash: Some(*hash),
            mtime: ts(1_700_000_001),
            etag: None,
            remote_item_id: None,
        };
        let entry = FileEntry {
            pair_id: pair.id,
            relative_path: rel_path,
            kind: FileKind::File,
            sync_state: FileSyncState::Clean,
            local: Some(side.clone()),
            remote: Some(side.clone()),
            synced: Some(side),
            pending_op_id: None,
            updated_at: ts(1_700_000_001),
        };
        store.file_entry_upsert(&entry).await.expect("entry upsert");
    }

    // Step 7: dirty query should be empty for Clean entries.
    let dirty = store
        .file_entries_dirty(&pair.id, 1000)
        .await
        .expect("dirty");
    assert!(
        dirty.is_empty(),
        "no Clean entry should appear in dirty index"
    );

    // Step 8: modify one file on disk; the new hash differs.
    let (changed_path, original_hash) = &observed[0];
    std::fs::write(changed_path, b"changed-bytes").expect("rewrite");
    let new_hash = fs.hash(&abs(changed_path)).await.expect("rehash");
    assert_ne!(&new_hash, original_hash, "hash should change");

    // Step 9: upsert that entry as Dirty.
    let changed_meta = std::fs::metadata(changed_path).expect("meta");
    let new_side = FileSide {
        kind: FileKind::File,
        size_bytes: changed_meta.len(),
        content_hash: Some(new_hash),
        mtime: ts(1_700_000_010),
        etag: None,
        remote_item_id: None,
    };
    let dirty_entry = FileEntry {
        pair_id: pair.id,
        relative_path: rel("f000.bin"),
        kind: FileKind::File,
        sync_state: FileSyncState::Dirty,
        local: Some(new_side.clone()),
        remote: None,
        synced: None,
        pending_op_id: None,
        updated_at: ts(1_700_000_010),
    };
    store
        .file_entry_upsert(&dirty_entry)
        .await
        .expect("dirty upsert");

    let dirty = store
        .file_entries_dirty(&pair.id, 1000)
        .await
        .expect("dirty");
    assert_eq!(dirty.len(), 1, "exactly the one Dirty entry should appear");
    assert_eq!(dirty[0].relative_path.as_str(), "f000.bin");
}

// Suppress unused import warning — LocalWriteStream is part of the port surface
// being validated in this acceptance test even though it is not called directly.
const _: fn() = || {
    let _: Option<LocalWriteStream> = None;
};
