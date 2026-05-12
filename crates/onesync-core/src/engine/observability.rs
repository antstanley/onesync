//! Helpers that build [`AuditEvent`] values emitted by the engine.

use onesync_protocol::{
    audit::AuditEvent,
    enums::AuditLevel,
    id::{AuditEventId, PairId},
    primitives::Timestamp,
};

/// Build an `AuditEvent` with the given fields.
///
/// The `id` is supplied by the caller (obtained from the `IdGenerator` port).
#[must_use]
pub fn make_event(
    id: AuditEventId,
    ts: Timestamp,
    level: AuditLevel,
    kind: impl Into<String>,
    pair_id: Option<PairId>,
    payload: serde_json::Map<String, serde_json::Value>,
) -> AuditEvent {
    AuditEvent {
        id,
        ts,
        level,
        kind: kind.into(),
        pair_id,
        payload,
    }
}

/// Build a cycle-started event.
#[must_use]
pub fn cycle_started(id: AuditEventId, ts: Timestamp, pair_id: PairId) -> AuditEvent {
    make_event(
        id,
        ts,
        AuditLevel::Info,
        "sync_cycle_started",
        Some(pair_id),
        serde_json::Map::new(),
    )
}

/// Build a cycle-finished event.
#[must_use]
pub fn cycle_finished(
    id: AuditEventId,
    ts: Timestamp,
    pair_id: PairId,
    ops_applied: usize,
    conflicts: usize,
) -> AuditEvent {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "ops_applied".to_owned(),
        serde_json::Value::Number(ops_applied.into()),
    );
    payload.insert(
        "conflicts_detected".to_owned(),
        serde_json::Value::Number(conflicts.into()),
    );
    make_event(
        id,
        ts,
        AuditLevel::Info,
        "sync_cycle_finished",
        Some(pair_id),
        payload,
    )
}

/// Build a file-op-failed event.
#[must_use]
pub fn op_failed(
    id: AuditEventId,
    ts: Timestamp,
    pair_id: PairId,
    relative_path: &str,
    reason: &str,
) -> AuditEvent {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "relative_path".to_owned(),
        serde_json::Value::String(relative_path.to_owned()),
    );
    payload.insert(
        "reason".to_owned(),
        serde_json::Value::String(reason.to_owned()),
    );
    make_event(
        id,
        ts,
        AuditLevel::Error,
        "file_op_failed",
        Some(pair_id),
        payload,
    )
}

/// Build a conflict-detected event.
#[must_use]
pub fn conflict_detected(
    id: AuditEventId,
    ts: Timestamp,
    pair_id: PairId,
    relative_path: &str,
) -> AuditEvent {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "relative_path".to_owned(),
        serde_json::Value::String(relative_path.to_owned()),
    );
    make_event(
        id,
        ts,
        AuditLevel::Warn,
        "conflict_detected",
        Some(pair_id),
        payload,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use onesync_protocol::{id::PairId, primitives::Timestamp};
    use ulid::Ulid;

    fn now() -> Timestamp {
        // LINT: Utc::now() is allowed in tests.
        #[allow(clippy::disallowed_methods)]
        Timestamp::from_datetime(Utc::now())
    }

    fn pair() -> PairId {
        // LINT: Ulid::new() is allowed in tests.
        #[allow(clippy::disallowed_methods)]
        PairId::from_ulid(Ulid::new())
    }

    fn audit_id() -> AuditEventId {
        // LINT: Ulid::new() is allowed in tests.
        #[allow(clippy::disallowed_methods)]
        AuditEventId::from_ulid(Ulid::new())
    }

    #[test]
    fn cycle_started_has_correct_kind() {
        let evt = cycle_started(audit_id(), now(), pair());
        assert_eq!(evt.kind, "sync_cycle_started");
        assert_eq!(evt.level, AuditLevel::Info);
    }

    #[test]
    fn cycle_finished_payload_contains_ops_applied() {
        let evt = cycle_finished(audit_id(), now(), pair(), 7, 1);
        assert_eq!(evt.payload["ops_applied"], 7);
        assert_eq!(evt.payload["conflicts_detected"], 1);
    }

    #[test]
    fn op_failed_is_error_level() {
        let evt = op_failed(audit_id(), now(), pair(), "docs/a.txt", "network timeout");
        assert_eq!(evt.level, AuditLevel::Error);
        assert_eq!(evt.payload["relative_path"], "docs/a.txt");
    }
}
