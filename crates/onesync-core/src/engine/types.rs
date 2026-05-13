//! Core data types shared across engine submodules.

use onesync_protocol::{
    enums::{ConflictSide, FileKind, FileOpKind},
    id::PairId,
    path::RelPath,
    primitives::Timestamp,
};

/// A single reconciliation decision produced by [`crate::engine::reconcile`].
///
/// Decisions are pure data — no I/O is performed. The planner converts them
/// into concrete [`FileOp`](onesync_protocol::file_op::FileOp) sequences.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// Sync pair this decision belongs to.
    pub pair_id: PairId,
    /// Path the decision concerns.
    pub relative_path: RelPath,
    /// What to do.
    pub kind: DecisionKind,
}

/// The action the engine has decided to take for one path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecisionKind {
    /// Upload the local file to remote.
    Upload,
    /// Download the remote file to local disk.
    Download,
    /// Delete the file on the local side.
    LocalDelete,
    /// Delete the file on the remote side.
    RemoteDelete,
    /// Create a folder on the local side.
    LocalMkdir,
    /// Create a folder on the remote side.
    RemoteMkdir,
    /// Rename the file on the local side.
    LocalRename {
        /// New path.
        new_path: RelPath,
    },
    /// Rename the file on the remote side.
    RemoteRename {
        /// New path.
        new_path: RelPath,
    },
    /// A conflict was detected; resolve by keeping the winner and renaming the loser.
    Conflict {
        /// Which side's content is kept at `relative_path`.
        winner: ConflictSide,
        /// Path where the losing side is saved as a conflict copy.
        loser_path: RelPath,
    },
    /// Both sides agree; nothing to do.
    NoOp,
}

impl DecisionKind {
    /// Map to the corresponding [`FileOpKind`] for non-conflict decisions.
    ///
    /// Returns `None` for [`DecisionKind::NoOp`] and [`DecisionKind::Conflict`].
    #[must_use]
    pub const fn to_file_op_kind(&self) -> Option<FileOpKind> {
        match self {
            Self::Upload => Some(FileOpKind::Upload),
            Self::Download => Some(FileOpKind::Download),
            Self::LocalDelete => Some(FileOpKind::LocalDelete),
            Self::RemoteDelete => Some(FileOpKind::RemoteDelete),
            Self::LocalMkdir => Some(FileOpKind::LocalMkdir),
            Self::RemoteMkdir => Some(FileOpKind::RemoteMkdir),
            Self::LocalRename { .. } => Some(FileOpKind::LocalRename),
            Self::RemoteRename { .. } => Some(FileOpKind::RemoteRename),
            Self::Conflict { .. } | Self::NoOp => None,
        }
    }
}

/// An ordered batch of decisions that the planner expands into `FileOp`s.
#[derive(Clone, Debug, Default)]
pub struct OpPlan {
    /// Decisions in execution order (mkdir before create, delete after move, etc.).
    pub decisions: Vec<Decision>,
}

/// Fatal or retriable errors produced by the engine.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// A port returned an error that the engine cannot recover from this cycle.
    #[error("port error: {0}")]
    Port(String),
    /// The engine was asked to shut down mid-cycle.
    #[error("shutting down")]
    Shutdown,
}

/// Summary produced at the end of a successful sync cycle.
#[derive(Debug, Default, Clone)]
pub struct CycleSummary {
    /// Number of items examined in the remote delta.
    pub remote_items_seen: usize,
    /// Number of local events processed.
    pub local_events_seen: usize,
    /// Number of file operations that were applied.
    pub ops_applied: usize,
    /// Number of conflicts detected this cycle.
    pub conflicts_detected: usize,
    /// New delta cursor returned by the terminal `/delta` page. `None` if the upstream
    /// adapter did not return a cursor (e.g. mid-stream paging error, fakes that don't
    /// emit one). The scheduler persists this on the `Pair` and uses its presence to
    /// gate the `Initializing -> Active` transition.
    pub delta_token: Option<onesync_protocol::primitives::DeltaCursor>,
}

/// Metadata about one file's mtime used by the conflict-detection heuristic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MtimePair {
    /// Kind of the entry (file vs. directory).
    pub kind: FileKind,
    /// Local mtime.
    pub local_mtime: Timestamp,
    /// Remote mtime.
    pub remote_mtime: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;
    use onesync_protocol::enums::FileOpKind;

    #[test]
    fn decision_kind_maps_to_file_op_kind() {
        assert_eq!(
            DecisionKind::Upload.to_file_op_kind(),
            Some(FileOpKind::Upload)
        );
        assert_eq!(
            DecisionKind::Download.to_file_op_kind(),
            Some(FileOpKind::Download)
        );
        assert_eq!(DecisionKind::NoOp.to_file_op_kind(), None);
        assert_eq!(
            DecisionKind::Conflict {
                winner: ConflictSide::Local,
                loser_path: "a.txt".parse().unwrap(),
            }
            .to_file_op_kind(),
            None
        );
    }
}
