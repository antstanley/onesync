//! RPC response shapes that wrap one or more entities.

use serde::{Deserialize, Serialize};

use crate::account::Account;
use crate::config::InstanceConfig;
use crate::conflict::Conflict;
use crate::file_op::FileOp;
use crate::id::SyncRunId;
use crate::pair::Pair;
use crate::sync_run::SyncRun;

/// Handle returned when a sync run is initiated via RPC, pairing the run's
/// identifier with the caller's subscription channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRunHandle {
    /// Identifier of the newly-started sync run.
    pub run_id: SyncRunId,
    /// Caller-visible subscription channel for progress events.
    pub subscription_id: String,
}

/// Acknowledgement returned when a subscription is successfully registered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionAck {
    /// The subscription channel identifier that was registered.
    pub subscription_id: String,
}

/// Aggregated status for a single sync pair, including in-flight work and
/// recent history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairStatusDetail {
    /// The sync pair this detail describes.
    pub pair: Pair,
    /// File operations currently executing or queued for this pair.
    pub in_flight_ops: Vec<FileOp>,
    /// The most recent sync runs for this pair, newest first.
    pub recent_runs: Vec<SyncRun>,
    /// Number of unresolved conflicts for this pair.
    pub conflict_count: u32,
    /// Number of file operations waiting in the queue.
    pub queue_depth: u32,
}

/// Full detail for a single sync run, including its individual operations and
/// any conflicts that arose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRunDetail {
    /// The sync run record.
    pub run: SyncRun,
    /// All file operations that belong to this run.
    pub ops: Vec<FileOp>,
    /// All conflicts detected during this run.
    pub conflicts: Vec<Conflict>,
}

/// Comprehensive daemon diagnostics snapshot, suitable for the `/diagnostics`
/// RPC endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostics {
    /// Application version string (e.g. `"0.1.0"`).
    pub version: String,
    /// Monotonically increasing schema version number.
    pub schema_version: u32,
    /// How long the daemon has been running, in seconds.
    pub uptime_s: u64,
    /// Status detail for every configured sync pair.
    pub pairs: Vec<PairStatusDetail>,
    /// All cloud-storage accounts linked to this instance.
    pub accounts: Vec<Account>,
    /// Current operator configuration for this instance.
    pub config: InstanceConfig,
    /// Number of active event subscriptions.
    pub subscriptions: u32,
}
