//! String-valued enums shared across the protocol.
//!
//! Every enum is `serde(rename_all = "snake_case")` to match the JSON Schema.

use serde::{Deserialize, Serialize};

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        #[allow(missing_docs)]
        pub enum $name {
            $($variant,)+
        }
    };
}

string_enum!(
    /// Whether an entry is a regular file or a directory.
    FileKind { File, Directory }
);
string_enum!(
    /// Whether an account is personal or business.
    AccountKind { Personal, Business }
);
string_enum!(
    /// Lifecycle state of a sync pair.
    PairStatus { Initializing, Active, Paused, Errored, Removed }
);
string_enum!(
    /// Sync state of an individual file.
    FileSyncState { Clean, Dirty, PendingUpload, PendingDownload, PendingConflict, InFlight }
);
string_enum!(
    /// Kind of file operation being performed.
    FileOpKind { Upload, Download, LocalDelete, RemoteDelete, LocalMkdir, RemoteMkdir, LocalRename, RemoteRename }
);
string_enum!(
    /// Execution status of a file operation.
    FileOpStatus { Enqueued, InProgress, Backoff, Success, Failed }
);
string_enum!(
    /// What triggered a sync run.
    RunTrigger { Scheduled, LocalEvent, RemoteWebhook, CliForce, BackoffRetry }
);
string_enum!(
    /// Overall outcome of a sync run.
    RunOutcome { Success, PartialFailure, Aborted }
);
string_enum!(
    /// Which side of a conflict is being referenced.
    ConflictSide { Local, Remote }
);
string_enum!(
    /// How a conflict was resolved.
    ConflictResolution { Auto, Manual }
);
string_enum!(
    /// Severity level for audit log entries.
    AuditLevel { Info, Warn, Error }
);
string_enum!(
    /// Verbosity level for log messages.
    LogLevel { Info, Debug, Trace }
);

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! round_trip {
        ($ty:ty, $variant:expr, $wire:expr) => {{
            let v: $ty = $variant;
            let json = serde_json::to_string(&v).expect("serialize");
            assert_eq!(json, format!("\"{}\"", $wire));
            let back: $ty = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, v);
        }};
    }

    #[test]
    fn file_kind_round_trip() {
        round_trip!(FileKind, FileKind::File, "file");
        round_trip!(FileKind, FileKind::Directory, "directory");
    }

    #[test]
    fn pair_status_round_trip_all_variants() {
        round_trip!(PairStatus, PairStatus::Initializing, "initializing");
        round_trip!(PairStatus, PairStatus::Active, "active");
        round_trip!(PairStatus, PairStatus::Paused, "paused");
        round_trip!(PairStatus, PairStatus::Errored, "errored");
        round_trip!(PairStatus, PairStatus::Removed, "removed");
    }

    #[test]
    fn file_op_kind_does_not_include_resolve_conflict() {
        let raw = "\"resolve_conflict\"";
        assert!(serde_json::from_str::<FileOpKind>(raw).is_err());
    }
}
