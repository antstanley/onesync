//! Audit-event builders for engine emissions.
//!
//! Every cycle, every op, and every error category produces a structured event.
//! Event `kind` values are the stable machine identifiers documented in
//! [`docs/spec/03-sync-engine.md`](../../../../docs/spec/03-sync-engine.md) §Observability.

use onesync_protocol::{
    audit::AuditEvent,
    enums::AuditLevel,
    id::{AuditTag, PairId},
};

use crate::ports::{AuditSink, Clock, IdGenerator};

/// Engine event kinds (stable; consumers parse them).
pub mod kinds {
    /// A sync cycle started.
    pub const CYCLE_START: &str = "cycle.start";
    /// A sync cycle finished.
    pub const CYCLE_FINISH: &str = "cycle.finish";
    /// Timing for a single cycle phase.
    pub const PHASE_TIMING: &str = "phase.timing";
    /// A `FileOp` was enqueued.
    pub const OP_ENQUEUED: &str = "op.enqueued";
    /// A `FileOp` started execution.
    pub const OP_STARTED: &str = "op.started";
    /// A `FileOp` finished successfully.
    pub const OP_FINISHED: &str = "op.finished";
    /// A `FileOp` failed.
    pub const OP_FAILED: &str = "op.failed";
    /// A conflict was detected between local and remote.
    pub const CONFLICT_DETECTED: &str = "conflict.detected";
    /// A conflict was resolved automatically by the keep-both policy.
    pub const CONFLICT_RESOLVED_AUTO: &str = "conflict.resolved.auto";
    /// A conflict was resolved manually by the operator.
    pub const CONFLICT_RESOLVED_MANUAL: &str = "conflict.resolved.manual";
    /// The local `FSEvents` queue overflowed; full rescan required.
    pub const LOCAL_FSEVENTS_OVERFLOW: &str = "local.fsevents.overflow";
    /// An engine concurrency limit was reached.
    pub const LIMIT_REACHED: &str = "limit.reached";
    /// A pair transitioned to the `Errored` state.
    pub const PAIR_ERRORED: &str = "pair.errored";
}

/// Emit an event through an [`AuditSink`].
///
/// `I` must implement [`IdGenerator`].  Uses a generic bound rather than
/// `&dyn IdGenerator` because [`IdGenerator::new_id`] takes a generic
/// parameter and the trait is not object-safe.
pub fn emit<I: IdGenerator>(
    sink: &dyn AuditSink,
    clock: &dyn Clock,
    ids: &I,
    level: AuditLevel,
    kind: &str,
    pair_id: Option<PairId>,
    payload: serde_json::Map<String, serde_json::Value>,
) {
    let event = AuditEvent {
        id: ids.new_id::<AuditTag>(),
        ts: clock.now(),
        level,
        kind: kind.to_owned(),
        pair_id,
        payload,
    };
    sink.emit(event);
}

/// Convenience wrapper to emit a `cycle.start` event.
pub fn emit_cycle_start<I: IdGenerator>(
    sink: &dyn AuditSink,
    clock: &dyn Clock,
    ids: &I,
    pair_id: PairId,
    trigger: &str,
) {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "trigger".into(),
        serde_json::Value::String(trigger.to_owned()),
    );
    emit(
        sink,
        clock,
        ids,
        AuditLevel::Info,
        kinds::CYCLE_START,
        Some(pair_id),
        payload,
    );
}

/// Convenience wrapper to emit a `cycle.finish` event.
pub fn emit_cycle_finish<I: IdGenerator>(
    sink: &dyn AuditSink,
    clock: &dyn Clock,
    ids: &I,
    pair_id: PairId,
    outcome: &str,
    local_ops: u32,
    remote_ops: u32,
) {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "outcome".into(),
        serde_json::Value::String(outcome.to_owned()),
    );
    payload.insert(
        "local_ops".into(),
        serde_json::Value::Number(serde_json::Number::from(local_ops)),
    );
    payload.insert(
        "remote_ops".into(),
        serde_json::Value::Number(serde_json::Number::from(remote_ops)),
    );
    emit(
        sink,
        clock,
        ids,
        AuditLevel::Info,
        kinds::CYCLE_FINISH,
        Some(pair_id),
        payload,
    );
}

/// Convenience wrapper to emit a `phase.timing` event.
pub fn emit_phase_timing<I: IdGenerator>(
    sink: &dyn AuditSink,
    clock: &dyn Clock,
    ids: &I,
    pair_id: PairId,
    phase: &str,
    elapsed_ms: u64,
) {
    let mut payload = serde_json::Map::new();
    payload.insert("phase".into(), serde_json::Value::String(phase.to_owned()));
    payload.insert(
        "elapsed_ms".into(),
        serde_json::Value::Number(serde_json::Number::from(elapsed_ms)),
    );
    emit(
        sink,
        clock,
        ids,
        AuditLevel::Info,
        kinds::PHASE_TIMING,
        Some(pair_id),
        payload,
    );
}

/// Convenience wrapper to emit a `pair.errored` event.
pub fn emit_pair_errored<I: IdGenerator>(
    sink: &dyn AuditSink,
    clock: &dyn Clock,
    ids: &I,
    pair_id: PairId,
    reason: &str,
) {
    let mut payload = serde_json::Map::new();
    payload.insert(
        "reason".into(),
        serde_json::Value::String(reason.to_owned()),
    );
    emit(
        sink,
        clock,
        ids,
        AuditLevel::Error,
        kinds::PAIR_ERRORED,
        Some(pair_id),
        payload,
    );
}
