//! Persisted conflict record.

use serde::{Deserialize, Serialize};

use crate::enums::{ConflictResolution, ConflictSide};
use crate::file_side::FileSide;
use crate::id::{ConflictId, PairId};
use crate::path::RelPath;
use crate::primitives::Timestamp;

/// A persisted record of a sync conflict and its resolution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    /// Unique identifier for this conflict record.
    pub id: ConflictId,
    /// Sync pair in which the conflict occurred.
    pub pair_id: PairId,
    /// Path of the winning version relative to the pair root.
    pub relative_path: RelPath,
    /// Which side's content was kept at `relative_path`.
    pub winner: ConflictSide,
    /// Path where the losing version was saved as a conflict copy.
    pub loser_relative_path: RelPath,
    /// Snapshot of the local file at conflict detection time.
    pub local_side: FileSide,
    /// Snapshot of the remote file at conflict detection time.
    pub remote_side: FileSide,
    /// When the conflict was first detected.
    pub detected_at: Timestamp,
    /// When the conflict was resolved; `None` if still open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<Timestamp>,
    /// How the conflict was resolved; `None` if still open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<ConflictResolution>,
    /// Optional operator note attached at resolution time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
