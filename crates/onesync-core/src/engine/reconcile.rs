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
    // If there's already an in-flight op or conflict, do nothing this cycle.
    match entry.sync_state {
        FileSyncState::InFlight | FileSyncState::PendingConflict => {
            return DecisionKind::NoOp;
        }
        FileSyncState::Clean
        | FileSyncState::Dirty
        | FileSyncState::PendingUpload
        | FileSyncState::PendingDownload => {}
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

        // Both changed → conflict.
        (true, true) => {
            // The caller resolves the conflict with pick_winner_and_loser; here we
            // just signal that one exists. The loser_path is filled by the planner.
            DecisionKind::Conflict {
                winner: onesync_protocol::enums::ConflictSide::Remote,
                loser_path: entry.relative_path.clone(),
            }
        }
    }
}

/// Returns `true` if the remote item differs from the synced snapshot.
fn remote_differs_from_synced(remote: &RemoteItem, synced: Option<&FileSide>) -> bool {
    let Some(synced) = synced else {
        // No synced snapshot yet means the remote is "new" to us.
        return true;
    };
    // Compare by remote ETag (fast) if present; fall back to size.
    if let Some(etag) = synced.etag.as_ref()
        && let Some(remote_etag) = remote.e_tag.as_deref()
    {
        return etag.as_str() != remote_etag;
    }
    synced.size_bytes != remote.size
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
        match d.kind {
            DecisionKind::Conflict { .. } => {}
            other => panic!("expected Conflict for initial-sync collision, got {other:?}"),
        }
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
