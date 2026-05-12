//! Engine-internal types.

use onesync_protocol::{
    enums::{RunOutcome, RunTrigger},
    id::{PairId, SyncRunId},
    path::RelPath,
    primitives::Timestamp,
};

use crate::ports::{GraphError, LocalFsError, StateError, VaultError};

/// What the engine decided to do about one path during reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Sides agree; no action required.
    Clean,
    /// Local content changed; upload to remote.
    UploadLocalToRemote,
    /// Remote content changed; download to local.
    DownloadRemoteToLocal,
    /// Local file removed; delete remote mirror.
    DeleteRemote,
    /// Remote file removed; delete local mirror.
    DeleteLocal,
    /// Both sides diverged from `synced`; apply keep-both conflict policy.
    Conflict {
        /// Side that wins the canonical path.
        winner: ConflictSide,
        /// The path the losing copy should be renamed to.
        loser_target: RelPath,
    },
}

/// Conflict winner — re-export of `onesync_protocol::enums::ConflictSide` for ergonomics.
pub use onesync_protocol::enums::ConflictSide;

/// Ordered list of operations to enqueue against a pair this cycle.
#[derive(Debug, Default, Clone)]
pub struct OpPlan {
    /// Operations in execution order. Directories before their files.
    pub ops: Vec<onesync_protocol::file_op::FileOp>,
    /// True if planning was truncated because `MAX_QUEUE_DEPTH_PER_PAIR` was reached.
    pub truncated: bool,
}

/// Summary returned by `run_cycle`.
#[derive(Debug, Clone)]
pub struct CycleSummary {
    /// Identifier of this sync run.
    pub run_id: SyncRunId,
    /// Pair that was synced.
    pub pair_id: PairId,
    /// What triggered this cycle.
    pub trigger: RunTrigger,
    /// When the cycle started.
    pub started_at: Timestamp,
    /// When the cycle finished.
    pub finished_at: Timestamp,
    /// Terminal outcome.
    pub outcome: RunOutcome,
    /// Number of operations applied to the local side.
    pub local_ops: u32,
    /// Number of operations applied to the remote side.
    pub remote_ops: u32,
    /// Total bytes uploaded.
    pub bytes_uploaded: u64,
    /// Total bytes downloaded.
    pub bytes_downloaded: u64,
}

/// Top-level engine error. Maps from any port error, then surfaced by `run_cycle`.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// State-store error.
    #[error("state: {0}")]
    State(#[from] StateError),
    /// Local-filesystem error.
    #[error("local fs: {0}")]
    LocalFs(#[from] LocalFsError),
    /// Microsoft Graph error.
    #[error("graph: {0}")]
    Graph(#[from] GraphError),
    /// Token vault error.
    #[error("vault: {0}")]
    Vault(#[from] VaultError),
    /// The pair is paused or in `Errored` state and the cycle was refused.
    #[error("pair not runnable: {0}")]
    PairNotRunnable(String),
    /// Cycle exceeded `CYCLE_PHASE_TIMEOUT_MS` somewhere.
    #[error("phase timeout: {phase}")]
    PhaseTimeout {
        /// The cycle phase that timed out.
        phase: &'static str,
    },
}
