//! Sync-cycle history entry.

use serde::{Deserialize, Serialize};

use crate::enums::{RunOutcome, RunTrigger};
use crate::id::{PairId, SyncRunId};
use crate::primitives::Timestamp;

/// A historical record of a single end-to-end sync cycle for a pair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRun {
    /// Unique identifier for this sync run.
    pub id: SyncRunId,
    /// Sync pair this run belongs to.
    pub pair_id: PairId,
    /// What caused this sync run to start.
    pub trigger: RunTrigger,
    /// When the run began.
    pub started_at: Timestamp,
    /// When the run ended; `None` while in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    /// Number of operations applied to the local side.
    pub local_ops: u32,
    /// Number of operations applied to the remote side.
    pub remote_ops: u32,
    /// Total bytes uploaded to `OneDrive` during this run.
    pub bytes_uploaded: u64,
    /// Total bytes downloaded from `OneDrive` during this run.
    pub bytes_downloaded: u64,
    /// Terminal outcome; `None` while in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RunOutcome>,
    /// Optional human-readable explanation of the outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_detail: Option<String>,
}
