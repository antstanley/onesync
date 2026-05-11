//! Structured-log entry persisted to the state store.

use serde::{Deserialize, Serialize};

use crate::enums::AuditLevel;
use crate::id::{AuditEventId, PairId};
use crate::primitives::Timestamp;

/// A single structured-log event written to the persistent audit trail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Unique identifier for this audit event.
    pub id: AuditEventId,
    /// Wall-clock time the event was recorded.
    pub ts: Timestamp,
    /// Severity of the event.
    pub level: AuditLevel,
    /// Machine-readable event kind (e.g. `"sync_run_started"`).
    pub kind: String,
    /// Sync pair this event is associated with, if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_id: Option<PairId>,
    /// Structured payload carrying event-specific fields.
    pub payload: serde_json::Map<String, serde_json::Value>,
}
