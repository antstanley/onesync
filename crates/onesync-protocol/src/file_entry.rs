//! Per-path sync state for one pair.

use serde::{Deserialize, Serialize};

use crate::enums::{FileKind, FileSyncState};
use crate::file_side::FileSide;
use crate::id::{FileOpId, PairId};
use crate::path::RelPath;
use crate::primitives::Timestamp;

/// The current sync state of a single path within a sync pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Sync pair this entry belongs to.
    pub pair_id: PairId,
    /// Path relative to the pair root.
    pub relative_path: RelPath,
    /// Whether the entry is a regular file or a directory.
    pub kind: FileKind,
    /// High-level lifecycle state of this entry.
    pub sync_state: FileSyncState,
    /// Local side snapshot; absent when the file does not exist locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<FileSide>,
    /// Remote side snapshot; absent when the file does not exist remotely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<FileSide>,
    /// Last successfully synced snapshot; absent on first sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced: Option<FileSide>,
    /// Id of the in-flight [`FileOp`](crate::file_op::FileOp), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_op_id: Option<FileOpId>,
    /// Wall-clock time this entry was last mutated in the state store.
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_entry_minimum_required_round_trips() {
        let raw = serde_json::json!({
            "pair_id": "pair_01J8X7CFGMZG7Y4DC0VA8DZW2H",
            "relative_path": "Documents/notes.md",
            "kind": "file",
            "sync_state": "clean",
            "updated_at": "2026-05-11T10:00:00Z"
        });
        let entry: FileEntry = serde_json::from_value(raw.clone()).expect("parses");
        assert_eq!(serde_json::to_value(&entry).unwrap(), raw);
    }
}
