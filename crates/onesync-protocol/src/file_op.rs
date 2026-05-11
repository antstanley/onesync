//! Discrete unit of sync work.

use serde::{Deserialize, Serialize};

use crate::enums::{FileOpKind, FileOpStatus};
use crate::errors::ErrorEnvelope;
use crate::id::{FileOpId, PairId, SyncRunId};
use crate::path::RelPath;
use crate::primitives::Timestamp;

/// A single file-level operation enqueued within a sync run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOp {
    /// Unique identifier for this operation.
    pub id: FileOpId,
    /// Sync run that owns this operation.
    pub run_id: SyncRunId,
    /// Sync pair this operation belongs to.
    pub pair_id: PairId,
    /// Path relative to the pair root.
    pub relative_path: RelPath,
    /// The action to perform (upload, download, delete, …).
    pub kind: FileOpKind,
    /// Current execution status.
    pub status: FileOpStatus,
    /// Number of times this operation has been attempted.
    pub attempts: u32,
    /// Most recent error, if any attempt failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<ErrorEnvelope>,
    /// Arbitrary key-value pairs for provider-specific metadata.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
    /// When this operation was first placed on the queue.
    pub enqueued_at: Timestamp,
    /// When the most recent execution attempt began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    /// When the operation reached a terminal status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
}
