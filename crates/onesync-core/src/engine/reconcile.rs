//! Pure reconciliation function: local state × remote delta → [`Decision`] list.
//!
//! Every function in this module is synchronous and infallible — all I/O has
//! already been performed; inputs are plain data values.

use onesync_protocol::{
    enums::{FileKind, FileSyncState},
    file_entry::FileEntry,
    file_side::FileSide,
    id::PairId,
    path::RelPath,
    remote::RemoteItem,
};

use crate::engine::types::{Decision, DecisionKind};

/// Reconcile one path's [`FileEntry`] (from the state store) against the
/// latest [`RemoteItem`] from a delta page.
///
/// Either argument may be `None`:
/// * `entry = None` — path is not yet tracked locally.
/// * `remote = None` — path was deleted from remote.
///
/// Returns a [`Decision`] describing what the engine should do. Pure: no I/O,
/// no side-effects.
#[must_use]
pub fn reconcile_one(
    pair_id: PairId,
    relative_path: RelPath,
    entry: Option<&FileEntry>,
    remote: Option<&RemoteItem>,
) -> Decision {
    let kind = reconcile_kind(entry, remote);
    Decision {
        pair_id,
        relative_path,
        kind,
    }
}

fn reconcile_kind(entry: Option<&FileEntry>, remote: Option<&RemoteItem>) -> DecisionKind {
    match (entry, remote) {
        // No local tracking, no remote item: nothing to do.
        (None, None) => DecisionKind::NoOp,

        // Remote item exists, nothing tracked locally → download (or mkdir).
        (None, Some(r)) => {
            if r.is_folder() {
                DecisionKind::LocalMkdir
            } else {
                DecisionKind::Download
            }
        }

        // Local tracking exists, remote item deleted → delete local copy.
        (Some(_entry), None) => DecisionKind::LocalDelete,

        // Both sides present.
        (Some(entry), Some(remote)) => reconcile_both(entry, remote),
    }
}

fn reconcile_both(entry: &FileEntry, remote: &RemoteItem) -> DecisionKind {
    if is_action_blocking(entry.sync_state) {
        return DecisionKind::NoOp;
    }

    let local_side: Option<&FileSide> = entry.local.as_ref();
    let synced_side: Option<&FileSide> = entry.synced.as_ref();

    // Did the remote change relative to the last synced snapshot?
    let remote_changed = remote_differs_from_synced(remote, synced_side);
    // Did the local side change relative to the last synced snapshot?
    let local_changed = local_differs_from_synced(local_side, synced_side);

    match (local_changed, remote_changed) {
        // Neither changed.
        (false, false) => DecisionKind::NoOp,

        // Only remote changed → download the new version.
        (false, true) => {
            if remote.is_folder() {
                DecisionKind::LocalMkdir
            } else {
                DecisionKind::Download
            }
        }

        // Only local changed → upload.
        (true, false) => {
            if entry.kind == FileKind::Directory {
                DecisionKind::RemoteMkdir
            } else {
                DecisionKind::Upload
            }
        }

        // Both changed → either content-equal (no-op, promote both to synced
        // on next persistence) or a genuine conflict.
        //
        // RP1-F1: spec `03-sync-engine.md` table line 140-147 splits the
        // (true,true) row into the case where the divergent sides agree with
        // each other (treat as NoOp; equivalent to "both sides converged")
        // and the case where they don't (Conflict).
        (true, true) => {
            if let Some(local) = local_side
                && local_equals_remote(local, remote)
            {
                return DecisionKind::NoOp;
            }
            // RP1-F3: emit the pre-policy `ConflictDetected` variant. The
            // conflict-resolution step in the cycle (per spec lines 194-208,
            // pending RP1-F4) will compute the winner + loser_path via
            // `pick_winner_and_loser` and re-express the decision as
            // `DecisionKind::Conflict`. The previous placeholder
            // (`winner: Remote, loser_path: relative_path`) was type-
            // indistinguishable from a resolved decision.
            DecisionKind::ConflictDetected
        }
    }
}

/// Whether a local [`FileSide`] is provably equal in content to a
/// [`RemoteItem`].
///
/// Strict: returns `true` only when we can prove equality, never on absence of
/// evidence. The cases we can prove with the metadata both sides expose:
///
/// - Both are directories.
/// - Both are zero-byte files (no content to disagree about).
/// - File sizes match AND the local-side `etag` equals the remote `e_tag`.
///
/// Hash comparison across local (BLAKE3) and remote (SHA-1 / `QuickXorHash`)
/// requires algorithm conversion we don't perform here — those cases fall
/// through and are surfaced as a `Conflict` so no data is silently dropped.
fn local_equals_remote(local: &FileSide, remote: &RemoteItem) -> bool {
    let local_is_folder = local.kind == FileKind::Directory;
    if local_is_folder != remote.is_folder() {
        return false;
    }
    if local_is_folder {
        return true;
    }
    if local.size_bytes != remote.size {
        return false;
    }
    if local.size_bytes == 0 {
        return true;
    }
    if let Some(local_etag) = local.etag.as_ref()
        && let Some(remote_etag) = remote.e_tag.as_deref()
        && local_etag.as_str() == remote_etag
    {
        return true;
    }
    false
}

/// RP1-F28: returns `true` when an existing `FileEntry`'s `sync_state`
/// blocks further engine action on the path this cycle.
///
/// Both `reconcile` and `phase_local_uploads` need this check; centralising
/// it here keeps the rule in one place. `InFlight` parks the path while an
/// op is mid-execute against the adapter; `PendingConflict` parks it while
/// an unresolved Conflict row is awaiting operator (or future-cycle)
/// action. All other states represent candidates the engine may act on.
#[must_use]
pub const fn is_action_blocking(state: FileSyncState) -> bool {
    matches!(
        state,
        FileSyncState::InFlight | FileSyncState::PendingConflict
    )
}

/// Returns `true` if the remote item differs from the synced snapshot.
///
/// RP1-F2/F6: when we cannot positively prove equality (no etag evidence, no
/// zero-byte short-circuit) we conservatively report `true`. The previous
/// behaviour fell back to size-only comparison which silently absorbed any
/// same-size remote edit (e.g. trivial text replacements that preserve byte
/// count). The trade-off is a false-positive download instead of a
/// false-negative missed change — the latter is data loss; the former is
/// merely extra bandwidth.
fn remote_differs_from_synced(remote: &RemoteItem, synced: Option<&FileSide>) -> bool {
    let Some(synced) = synced else {
        // No synced snapshot yet means the remote is "new" to us.
        return true;
    };
    // RP1-F30: spec `03-sync-engine.md` line 149 defines equality as
    // `(kind, size_bytes, content_hash)`. A path that flipped from File to
    // Directory (or vice versa) is divergent even when etag/size happen to
    // line up. `is_folder()` is the only kind signal we have remote-side.
    let synced_is_folder = synced.kind == FileKind::Directory;
    if synced_is_folder != remote.is_folder() {
        return true;
    }
    // Strongest signal: matching etag proves equality, mismatching proves
    // divergence. Either way we can return a definitive answer.
    if let Some(etag) = synced.etag.as_ref()
        && let Some(remote_etag) = remote.e_tag.as_deref()
    {
        return etag.as_str() != remote_etag;
    }
    // Sizes differ → definitely divergent.
    if synced.size_bytes != remote.size {
        return true;
    }
    // Zero-byte file with matching size has no content to disagree about.
    if synced.size_bytes == 0 {
        return false;
    }
    // Sizes match, non-zero bytes, no etag pair available. The cross-algorithm
    // hash comparison (BLAKE3 vs SHA-1/QuickXorHash) we don't yet perform is
    // the only remaining equality signal — without it, assume divergence.
    true
}

/// Returns `true` if the local side differs from the synced snapshot.
fn local_differs_from_synced(local: Option<&FileSide>, synced: Option<&FileSide>) -> bool {
    match (local, synced) {
        (None, None) => false,
        // local deleted or local appeared
        (None, Some(_)) | (Some(_), None) => true,
        (Some(l), Some(s)) => !l.identifies_same_content_as(s),
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use onesync_protocol::{
        enums::{FileKind, FileSyncState},
        file_entry::FileEntry,
        id::PairId,
        path::RelPath,
        primitives::Timestamp,
        remote::RemoteItem,
    };
    use ulid::Ulid;

    fn pair() -> PairId {
        // LINT: Ulid::new() is allowed in tests.
        #[allow(clippy::disallowed_methods)]
        PairId::from_ulid(Ulid::new())
    }

    fn path(s: &str) -> RelPath {
        s.parse().unwrap()
    }

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_datetime(Utc.timestamp_opt(secs, 0).unwrap())
    }

    fn remote_file(id: &str, name: &str, size: u64) -> RemoteItem {
        RemoteItem {
            id: id.to_owned(),
            name: name.to_owned(),
            size,
            e_tag: None,
            c_tag: None,
            last_modified_date_time: None,
            file: Some(onesync_protocol::remote::FileFacet {
                hashes: onesync_protocol::remote::FileHashes::default(),
            }),
            folder: None,
            deleted: None,
            parent_reference: None,
        }
    }

    fn remote_folder(id: &str, name: &str) -> RemoteItem {
        RemoteItem {
            id: id.to_owned(),
            name: name.to_owned(),
            size: 0,
            e_tag: None,
            c_tag: None,
            last_modified_date_time: None,
            file: None,
            folder: Some(onesync_protocol::remote::FolderFacet { child_count: 0 }),
            deleted: None,
            parent_reference: None,
        }
    }

    fn blank_entry(pair_id: PairId, relative_path: RelPath) -> FileEntry {
        FileEntry {
            pair_id,
            relative_path,
            kind: FileKind::File,
            sync_state: FileSyncState::Clean,
            local: None,
            remote: None,
            synced: None,
            pending_op_id: None,
            updated_at: ts(0),
        }
    }

    #[test]
    fn no_entry_no_remote_is_noop() {
        let d = reconcile_one(pair(), path("a.txt"), None, None);
        assert_eq!(d.kind, DecisionKind::NoOp);
    }

    #[test]
    fn new_remote_file_produces_download() {
        let remote = remote_file("r1", "a.txt", 100);
        let d = reconcile_one(pair(), path("a.txt"), None, Some(&remote));
        assert_eq!(d.kind, DecisionKind::Download);
    }

    #[test]
    fn new_remote_folder_produces_local_mkdir() {
        let remote = remote_folder("r2", "docs");
        let d = reconcile_one(pair(), path("docs"), None, Some(&remote));
        assert_eq!(d.kind, DecisionKind::LocalMkdir);
    }

    #[test]
    fn remote_deleted_produces_local_delete() {
        let p = pair();
        let rp = path("a.txt");
        let entry = blank_entry(p, rp.clone());
        let d = reconcile_one(p, rp, Some(&entry), None);
        assert_eq!(d.kind, DecisionKind::LocalDelete);
    }

    #[test]
    fn initial_sync_collision_is_conflict() {
        // M12b initial-sync case: phase_local_uploads has populated entry.local from
        // a local scan but no synced snapshot exists yet, and the same path appears in
        // the remote delta. Without hashes to compare, the engine reports a conflict
        // rather than silently picking a side.
        let p = pair();
        let rp = path("collide.txt");
        let mut entry = blank_entry(p, rp.clone());
        entry.local = Some(FileSide {
            kind: FileKind::File,
            size_bytes: 5,
            content_hash: None,
            mtime: ts(100),
            etag: None,
            remote_item_id: None,
        });
        entry.synced = None;
        let remote = remote_file("r1", "collide.txt", 5);
        let d = reconcile_one(p, rp, Some(&entry), Some(&remote));
        assert!(
            d.kind.is_conflict(),
            "expected a conflict variant for initial-sync collision, got {:?}",
            d.kind
        );
    }

    /// Helper: a non-empty content hash (`FileSide::identifies_same_content_as`
    /// debug-asserts that non-directory sides carry a hash).
    fn h(byte: u8) -> onesync_protocol::primitives::ContentHash {
        let hex: String = std::iter::repeat_n(format!("{byte:02x}"), 32).collect();
        hex.parse().unwrap()
    }

    /// RP1-F1: when both sides diverged from `synced` but agree with each
    /// other (here, both truncated to zero bytes), the engine emits `NoOp`
    /// rather than a spurious `Conflict`.
    #[test]
    fn both_diverged_but_zero_byte_equal_is_noop() {
        let p = pair();
        let rp = path("a.txt");
        let mut entry = blank_entry(p, rp.clone());
        entry.local = Some(FileSide {
            kind: FileKind::File,
            size_bytes: 0,
            content_hash: Some(h(0x00)),
            mtime: ts(200),
            etag: None,
            remote_item_id: None,
        });
        entry.synced = Some(FileSide {
            kind: FileKind::File,
            size_bytes: 42,
            content_hash: Some(h(0x11)),
            mtime: ts(100),
            etag: Some(onesync_protocol::primitives::ETag::new("v0")),
            remote_item_id: None,
        });
        let remote = remote_file("r1", "a.txt", 0);
        let d = reconcile_one(p, rp, Some(&entry), Some(&remote));
        assert_eq!(
            d.kind,
            DecisionKind::NoOp,
            "both sides truncated to zero must reconcile as NoOp"
        );
    }

    /// RP1-F1 sibling: divergent sides with matching etag also reconcile as
    /// `NoOp` (the only "positive evidence of equality" we have when sizes are
    /// non-zero and content hashes don't align across algorithms).
    #[test]
    fn both_diverged_with_matching_etag_is_noop() {
        let p = pair();
        let rp = path("a.txt");
        let mut entry = blank_entry(p, rp.clone());
        entry.local = Some(FileSide {
            kind: FileKind::File,
            size_bytes: 200,
            content_hash: Some(h(0xaa)),
            mtime: ts(200),
            etag: Some(onesync_protocol::primitives::ETag::new("v2")),
            remote_item_id: None,
        });
        entry.synced = Some(FileSide {
            kind: FileKind::File,
            size_bytes: 100,
            content_hash: Some(h(0xbb)),
            mtime: ts(100),
            etag: Some(onesync_protocol::primitives::ETag::new("v0")),
            remote_item_id: None,
        });
        let mut remote = remote_file("r1", "a.txt", 200);
        remote.e_tag = Some("v2".to_owned());
        let d = reconcile_one(p, rp, Some(&entry), Some(&remote));
        assert_eq!(d.kind, DecisionKind::NoOp);
    }

    /// Negative control: when neither size match nor etag match holds, the
    /// engine still reports `ConflictDetected` — F1's strict equality
    /// preserves the "no silent data drop" guarantee.
    #[test]
    fn both_diverged_without_evidence_remains_conflict() {
        let p = pair();
        let rp = path("a.txt");
        let mut entry = blank_entry(p, rp.clone());
        entry.local = Some(FileSide {
            kind: FileKind::File,
            size_bytes: 200,
            content_hash: Some(h(0xaa)),
            mtime: ts(200),
            etag: None,
            remote_item_id: None,
        });
        entry.synced = Some(FileSide {
            kind: FileKind::File,
            size_bytes: 100,
            content_hash: Some(h(0xbb)),
            mtime: ts(100),
            etag: Some(onesync_protocol::primitives::ETag::new("v0")),
            remote_item_id: None,
        });
        let mut remote = remote_file("r1", "a.txt", 200);
        remote.e_tag = Some("v9".to_owned());
        let d = reconcile_one(p, rp, Some(&entry), Some(&remote));
        assert!(
            d.kind.is_conflict(),
            "expected a conflict variant, got {:?}",
            d.kind
        );
    }

    /// RP1-F2/F6: when `synced.etag` is `None` (initial cycle, post-clear, or
    /// any cycle where the prior delta page omitted etag) a same-size remote
    /// edit was previously invisible. After the fix, absence of etag evidence
    /// forces a `Download` decision rather than silently treating the remote
    /// as unchanged.
    #[test]
    fn remote_same_size_without_etag_triggers_download() {
        let p = pair();
        let rp = path("a.txt");
        let mut entry = blank_entry(p, rp.clone());
        // Local matches synced — only the remote side could be divergent.
        let local_synced = FileSide {
            kind: FileKind::File,
            size_bytes: 100,
            content_hash: Some(h(0xaa)),
            mtime: ts(100),
            etag: None,
            remote_item_id: None,
        };
        entry.local = Some(local_synced.clone());
        entry.synced = Some(local_synced);
        // Remote has the same size but a fresh etag — sizes alone can't tell
        // us if the bytes changed, so the engine must download.
        let mut remote = remote_file("r1", "a.txt", 100);
        remote.e_tag = Some("v9".to_owned());
        let d = reconcile_one(p, rp, Some(&entry), Some(&remote));
        assert_eq!(
            d.kind,
            DecisionKind::Download,
            "same-size remote item with unknown etag must conservatively download"
        );
    }

    /// RP1-F2/F6 sibling: when the remote etag matches the synced etag, the
    /// engine still proves equality and returns `NoOp` (the fast-path).
    #[test]
    fn remote_with_matching_etag_is_noop() {
        let p = pair();
        let rp = path("a.txt");
        let mut entry = blank_entry(p, rp.clone());
        let local_synced = FileSide {
            kind: FileKind::File,
            size_bytes: 100,
            content_hash: Some(h(0xaa)),
            mtime: ts(100),
            etag: Some(onesync_protocol::primitives::ETag::new("v3")),
            remote_item_id: None,
        };
        entry.local = Some(local_synced.clone());
        entry.synced = Some(local_synced);
        let mut remote = remote_file("r1", "a.txt", 100);
        remote.e_tag = Some("v3".to_owned());
        let d = reconcile_one(p, rp, Some(&entry), Some(&remote));
        assert_eq!(d.kind, DecisionKind::NoOp);
    }

    /// RP1-F30: a path that synced as a File but now appears as a Directory
    /// (or vice versa) is divergent — even when size/etag happen to line up.
    /// Isolate the kind check by using a zero-byte file vs a folder (both
    /// have `size = 0`, so the existing size and zero-byte short-circuits
    /// don't fire).
    #[test]
    fn kind_flip_file_to_directory_is_remote_divergence() {
        let p = pair();
        let rp = path("kind");
        let mut entry = blank_entry(p, rp.clone());
        // FileSide::identifies_same_content_as debug-asserts non-directory
        // sides carry a hash; supply one so local-vs-synced equality
        // returns false (no local change), isolating the remote kind flip.
        entry.synced = Some(FileSide {
            kind: FileKind::File,
            size_bytes: 0,
            content_hash: Some(h(0xaa)),
            mtime: ts(100),
            etag: None,
            remote_item_id: None,
        });
        entry.local = entry.synced.clone();
        // Remote returns a Directory at the same path — same size (0) and
        // no etag, but the kind has flipped.
        let remote = remote_folder("r1", "kind");
        let d = reconcile_one(p, rp, Some(&entry), Some(&remote));
        // local matches synced (no local change), remote diverges (kind
        // flipped File→Directory) → LocalMkdir per the reconcile table
        // ((false, true) with folder => LocalMkdir).
        assert_eq!(d.kind, DecisionKind::LocalMkdir);
    }

    #[test]
    fn inflight_entry_is_noop() {
        let p = pair();
        let rp = path("a.txt");
        let mut entry = blank_entry(p, rp.clone());
        entry.sync_state = FileSyncState::InFlight;
        let remote = remote_file("r1", "a.txt", 100);
        let d = reconcile_one(p, rp, Some(&entry), Some(&remote));
        assert_eq!(d.kind, DecisionKind::NoOp);
    }
}
