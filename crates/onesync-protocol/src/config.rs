//! Operator-tunable instance configuration.

use serde::{Deserialize, Serialize};

use crate::enums::LogLevel;
use crate::primitives::Timestamp;

/// Operator-tunable settings for the running onesync instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceConfig {
    /// Minimum verbosity level for log output.
    pub log_level: LogLevel,
    /// Whether to emit macOS user notifications for sync events.
    pub notify: bool,
    /// Whether syncing is permitted over metered network connections.
    pub allow_metered: bool,
    /// Minimum local free disk space (GiB) before sync is paused.
    pub min_free_gib: u32,
    /// When this configuration was last written.
    pub updated_at: Timestamp,
}
