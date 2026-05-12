//! Property tests for the `reconcile` decision table.
//!
//! Generates random `(synced, local, remote)` triples and asserts that
//! `reconcile` produces a `Decision` consistent with the spec table.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::collections::BTreeSet;

use chrono::{TimeZone, Utc};
use onesync_core::engine::reconcile::reconcile;
use onesync_core::engine::types::Decision;
use onesync_protocol::{
    enums::FileKind,
    file_side::FileSide,
    path::RelPath,
    primitives::{ContentHash, Timestamp},
};
use proptest::prelude::*;

fn ts(secs: i64) -> Timestamp {
    Timestamp::from_datetime(Utc.timestamp_opt(secs, 0).unwrap())
}

/// Generate an arbitrary `FileSide`.
fn arb_file_side(seed: u8, mtime_secs: i64) -> FileSide {
    FileSide {
        kind: FileKind::File,
        size_bytes: u64::from(seed) * 100,
        content_hash: Some(ContentHash::from_bytes([seed; 32])),
        mtime: ts(mtime_secs),
        etag: None,
        remote_item_id: None,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..Default::default() })]

    /// Row 1: synced == local == remote → Clean.
    #[test]
    fn all_equal_is_clean(seed in 1u8..=3u8, mtime in 100i64..=300i64) {
        let side = arb_file_side(seed, mtime);
        let path: RelPath = "a.txt".parse().unwrap();
        let d = reconcile(
            &path, Some(&side), Some(&side), Some(&side),
            "host", ts(0), &BTreeSet::new(),
        );
        prop_assert_eq!(d, Decision::Clean);
    }

    /// Row 2: local differs, remote == synced → UploadLocalToRemote (unless local is None).
    #[test]
    fn only_local_differs_yields_upload_or_delete_remote(mtime in 100i64..500i64) {
        let synced = arb_file_side(1, 100);
        let local = arb_file_side(2, mtime); // always different from synced (seed 2 vs 1)
        let path: RelPath = "b.txt".parse().unwrap();
        let d = reconcile(
            &path, Some(&synced), Some(&local), Some(&synced),
            "host", ts(0), &BTreeSet::new(),
        );
        prop_assert_eq!(d, Decision::UploadLocalToRemote);
    }

    /// Row 3: remote differs, local == synced → DownloadRemoteToLocal.
    #[test]
    fn only_remote_differs_yields_download(mtime in 100i64..500i64) {
        let synced = arb_file_side(1, 100);
        let remote = arb_file_side(3, mtime);
        let path: RelPath = "c.txt".parse().unwrap();
        let d = reconcile(
            &path, Some(&synced), Some(&synced), Some(&remote),
            "host", ts(0), &BTreeSet::new(),
        );
        prop_assert_eq!(d, Decision::DownloadRemoteToLocal);
    }

    /// Row 4: local removed (synced present, local None, remote == synced) → DeleteRemote.
    #[test]
    fn local_removed_yields_delete_remote(seed in 1u8..=3u8) {
        let synced = arb_file_side(seed, 100);
        let path: RelPath = "d.txt".parse().unwrap();
        let d = reconcile(
            &path, Some(&synced), None, Some(&synced),
            "host", ts(0), &BTreeSet::new(),
        );
        prop_assert_eq!(d, Decision::DeleteRemote);
    }

    /// Row 5: remote removed (synced present, remote None, local == synced) → DeleteLocal.
    #[test]
    fn remote_removed_yields_delete_local(seed in 1u8..=3u8) {
        let synced = arb_file_side(seed, 100);
        let path: RelPath = "e.txt".parse().unwrap();
        let d = reconcile(
            &path, Some(&synced), Some(&synced), None,
            "host", ts(0), &BTreeSet::new(),
        );
        prop_assert_eq!(d, Decision::DeleteLocal);
    }

    /// Both sides differ from synced and from each other → Conflict (or Clean if converged).
    #[test]
    fn both_differ_is_conflict_or_clean(
        local_seed in 2u8..=3u8,
        remote_seed in 2u8..=3u8,
        local_mtime in 100i64..1000i64,
        remote_mtime in 100i64..1000i64,
    ) {
        let synced = arb_file_side(1, 50);
        let local = arb_file_side(local_seed, local_mtime);
        let remote = arb_file_side(remote_seed, remote_mtime);
        let path: RelPath = "f.txt".parse().unwrap();
        let d = reconcile(
            &path, Some(&synced), Some(&local), Some(&remote),
            "host", ts(0), &BTreeSet::new(),
        );
        match d {
            Decision::Clean => {
                // Only valid if local and remote content are the same.
                prop_assert!(
                    local.content_hash == remote.content_hash,
                    "Clean only when content hashes match; got local={:?} remote={:?}",
                    local.content_hash,
                    remote.content_hash,
                );
            }
            Decision::Conflict { .. } => {} // valid
            other => prop_assert!(false, "unexpected decision {:?}", other),
        }
    }

    /// No synced: if local is None, remote is Some → DownloadRemoteToLocal.
    #[test]
    fn no_synced_local_absent_remote_present_yields_download(seed in 1u8..=3u8) {
        let remote = arb_file_side(seed, 100);
        let path: RelPath = "g.txt".parse().unwrap();
        let d = reconcile(
            &path, None, None, Some(&remote),
            "host", ts(0), &BTreeSet::new(),
        );
        prop_assert_eq!(d, Decision::DownloadRemoteToLocal);
    }

    /// No synced: if local is Some, remote is None → UploadLocalToRemote.
    #[test]
    fn no_synced_local_present_remote_absent_yields_upload(seed in 1u8..=3u8) {
        let local = arb_file_side(seed, 100);
        let path: RelPath = "h.txt".parse().unwrap();
        let d = reconcile(
            &path, None, Some(&local), None,
            "host", ts(0), &BTreeSet::new(),
        );
        prop_assert_eq!(d, Decision::UploadLocalToRemote);
    }
}
