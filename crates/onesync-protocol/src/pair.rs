//! Pair entity.

use serde::{Deserialize, Serialize};

use crate::enums::PairStatus;
use crate::id::{AccountId, PairId};
use crate::path::AbsPath;
use crate::primitives::{DeltaCursor, DriveItemId, Timestamp};

/// A sync pair linking a local folder to a remote `OneDrive` folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    /// Unique pair identifier.
    pub id: PairId,
    /// The account this pair belongs to.
    pub account_id: AccountId,
    /// Absolute local filesystem path being synced.
    pub local_path: AbsPath,
    /// `OneDrive` driveItem identifier of the remote root.
    pub remote_item_id: DriveItemId,
    /// Human-readable remote path for display purposes.
    pub remote_path: String,
    /// Human-readable name for this pair.
    pub display_name: String,
    /// Current lifecycle state of this pair.
    pub status: PairStatus,
    /// Whether syncing is paused for this pair.
    pub paused: bool,
    /// Opaque delta cursor from the last successful /delta call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_token: Option<DeltaCursor>,
    /// Human-readable reason if the pair is in the errored state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errored_reason: Option<String>,
    /// When this pair record was created.
    pub created_at: Timestamp,
    /// When this pair record was last updated.
    pub updated_at: Timestamp,
    /// When the last successful sync completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<Timestamp>,
    /// Number of unresolved conflicts for this pair.
    pub conflict_count: u32,
    /// Per-pair opt-in for Graph `/subscriptions` push delivery. Polling is the always-on
    /// fallback; this flag only governs whether the daemon registers a subscription.
    #[serde(default)]
    pub webhook_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_round_trips_through_json() {
        let raw = serde_json::json!({
            "id": "pair_01J8X7CFGMZG7Y4DC0VA8DZW2H",
            "account_id": "acct_01J8X7CFGMZG7Y4DC0VA8DZW2H",
            "local_path": "/Users/alice/OneDrive",
            "remote_item_id": "drive-item-root",
            "remote_path": "/",
            "display_name": "OneDrive",
            "status": "active",
            "paused": false,
            "created_at": "2026-05-11T10:00:00Z",
            "updated_at": "2026-05-11T10:00:00Z",
            "conflict_count": 0,
            "webhook_enabled": false
        });
        let pair: Pair = serde_json::from_value(raw.clone()).expect("parses");
        assert_eq!(serde_json::to_value(&pair).unwrap(), raw);
    }
}
