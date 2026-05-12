//! `run_cycle`: the six-phase sync-cycle driver.
//!
//! Phases:
//! 1. **Delta** — fetch remote changes since the last cursor.
//! 2. **Events** — drain pending local filesystem events.
//! 3. **Reconcile** — pure: for each changed path produce a [`Decision`].
//! 4. **Plan** — convert decisions into ordered [`FileOp`]s.
//! 5. **Execute** — drive each op through the port layer.
//! 6. **Record** — persist the `SyncRun` and emit audit events.

use onesync_protocol::{
    enums::{FileOpStatus, RunOutcome, RunTrigger},
    file_op::FileOp,
    id::{AuditEventId, SyncRunId},
    path::RelPath,
    primitives::DeltaCursor,
    sync_run::SyncRun,
};

use crate::{
    engine::{
        executor::{execute, is_retriable},
        observability::{cycle_finished, cycle_started, op_failed},
        planner::plan,
        reconcile::reconcile_one,
        retry::{RetryDecision, retry_decision},
        types::{CycleSummary, DecisionKind, EngineError},
    },
    ports::{AuditSink, Clock, IdGenerator, LocalFs, RemoteDrive, StateStore},
};

/// Context required to run one sync cycle.
pub struct CycleCtx<'a, I: IdGenerator> {
    /// Sync pair id.
    pub pair_id: onesync_protocol::id::PairId,
    /// Absolute path on disk to the pair's local root.
    pub local_root: onesync_protocol::path::AbsPath,
    /// Graph drive id for this pair.
    pub drive_id: onesync_protocol::primitives::DriveId,
    /// Current delta cursor; `None` forces a full rescan.
    pub cursor: Option<DeltaCursor>,
    /// What triggered this cycle.
    pub trigger: RunTrigger,
    /// State store (port).
    pub state: &'a dyn StateStore,
    /// Remote drive (port).
    pub remote: &'a dyn RemoteDrive,
    /// Local filesystem (port).
    pub local: &'a dyn LocalFs,
    /// Audit sink (port).
    pub audit: &'a dyn AuditSink,
    /// Clock (port).
    pub clock: &'a dyn Clock,
    /// Id generator (port).
    pub ids: &'a I,
    /// Hostname used in conflict-copy filenames.
    pub host_name: String,
}

/// Run one full sync cycle.
///
/// # Errors
///
/// Returns [`EngineError::Port`] if a fatal port call fails.
/// Returns [`EngineError::Shutdown`] if a shutdown signal is detected (future).
pub async fn run_cycle<I: IdGenerator>(ctx: &CycleCtx<'_, I>) -> Result<CycleSummary, EngineError> {
    let now = ctx.clock.now();
    let run_id: SyncRunId = ctx.ids.new_id();
    let started_at = now;

    ctx.audit
        .emit(cycle_started(ctx.ids.new_id(), now, ctx.pair_id));

    // Phase 1 + 2 + 3: delta → reconcile → decisions.
    let (decisions, conflicts_detected, remote_items_seen) = phase_delta_reconcile(ctx).await?;

    // Phase 4: plan.
    let ops = plan(decisions, run_id, now, ctx.ids);

    // Phase 5: execute.
    let ops_applied = phase_execute(ctx, ops).await?;

    // Phase 6: record.
    let finished_at = ctx.clock.now();
    // LINT: ops_applied ≤ MAX_QUEUE_DEPTH_PER_PAIR (4096) so truncation is safe.
    #[allow(clippy::cast_possible_truncation)]
    let run = SyncRun {
        id: run_id,
        pair_id: ctx.pair_id,
        trigger: ctx.trigger,
        outcome: Some(RunOutcome::Success),
        outcome_detail: None,
        local_ops: ops_applied as u32,
        remote_ops: 0,
        bytes_uploaded: 0,
        bytes_downloaded: 0,
        started_at,
        finished_at: Some(finished_at),
    };
    ctx.state
        .run_record(&run)
        .await
        .map_err(|e| EngineError::Port(e.to_string()))?;

    let summary = CycleSummary {
        remote_items_seen,
        local_events_seen: 0,
        ops_applied,
        conflicts_detected,
    };

    ctx.audit.emit(cycle_finished(
        ctx.ids.new_id(),
        finished_at,
        ctx.pair_id,
        summary.ops_applied,
        summary.conflicts_detected,
    ));

    Ok(summary)
}

/// Phase 1–3: fetch delta, reconcile each item, return (decisions, conflicts, `items_seen`).
async fn phase_delta_reconcile<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
) -> Result<(Vec<crate::engine::types::Decision>, usize, usize), EngineError> {
    let delta_page = ctx
        .remote
        .delta(&ctx.drive_id, ctx.cursor.as_ref())
        .await
        .map_err(|e| EngineError::Port(e.to_string()))?;

    let remote_items_seen = delta_page.items.len();
    let mut decisions = Vec::new();
    let mut conflicts_detected = 0usize;

    for remote_item in &delta_page.items {
        let Ok(rel_path) = remote_item.name.parse::<RelPath>() else {
            continue;
        };

        let entry = ctx
            .state
            .file_entry_get(&ctx.pair_id, &rel_path)
            .await
            .map_err(|e| EngineError::Port(e.to_string()))?;

        let remote_opt = if remote_item.is_deleted() {
            None
        } else {
            Some(remote_item)
        };

        let decision = reconcile_one(ctx.pair_id, rel_path, entry.as_ref(), remote_opt);

        if matches!(decision.kind, DecisionKind::Conflict { .. }) {
            conflicts_detected += 1;
        }

        decisions.push(decision);
    }

    Ok((decisions, conflicts_detected, remote_items_seen))
}

/// Phase 5: execute each op with retries; returns the count of ops that succeeded.
async fn phase_execute<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
    ops: Vec<FileOp>,
) -> Result<usize, EngineError> {
    let mut ops_applied = 0usize;

    for mut op in ops {
        ctx.state
            .op_insert(&op)
            .await
            .map_err(|e| EngineError::Port(e.to_string()))?;

        let mut attempt: u32 = 0;
        loop {
            match retry_decision(attempt, pseudo_jitter(attempt)) {
                RetryDecision::Exhausted => {
                    ctx.state
                        .op_update_status(&op.id, FileOpStatus::Failed)
                        .await
                        .map_err(|e| EngineError::Port(e.to_string()))?;
                    break;
                }
                RetryDecision::Immediate | RetryDecision::Backoff { .. } => {}
            }

            op.attempts = attempt + 1;
            match execute(&op, &ctx.local_root, ctx.local, ctx.remote).await {
                Ok(status) => {
                    ctx.state
                        .op_update_status(&op.id, status)
                        .await
                        .map_err(|e| EngineError::Port(e.to_string()))?;
                    ops_applied += 1;
                    break;
                }
                Err(e) if is_retriable(&e) => {
                    attempt += 1;
                }
                Err(e) => {
                    ctx.state
                        .op_update_status(&op.id, FileOpStatus::Failed)
                        .await
                        .map_err(|e2| EngineError::Port(e2.to_string()))?;
                    let fail_id: AuditEventId = ctx.ids.new_id();
                    ctx.audit.emit(op_failed(
                        fail_id,
                        ctx.clock.now(),
                        ctx.pair_id,
                        op.relative_path.as_str(),
                        &e.to_string(),
                    ));
                    break;
                }
            }
        }
    }

    Ok(ops_applied)
}

/// Deterministic pseudo-jitter for use without a random source.
///
/// Returns 0.25 for odd attempts, 0.0 for even — purely for retry scheduling in
/// deterministic contexts (tests, and as a fallback). Production callers should
/// supply true random jitter.
const fn pseudo_jitter(attempt: u32) -> f64 {
    if attempt % 2 == 1 { 0.25 } else { 0.0 }
}
