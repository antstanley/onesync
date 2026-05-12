//! Six-phase sync cycle driver.
//!
//! Implements `run_cycle` which orchestrates local scan, remote delta, reconcile,
//! plan, and execute phases for a single pair in one atomic sweep.
//!
//! See [`docs/spec/03-sync-engine.md`](../../../../docs/spec/03-sync-engine.md) §Cycle structure.

use std::collections::BTreeSet;
use std::time::Duration;

use onesync_protocol::{
    enums::{FileSyncState, PairStatus, RunOutcome, RunTrigger},
    file_entry::FileEntry,
    id::{PairId, SyncRunTag},
    pair::Pair,
    path::RelPath,
    primitives::Timestamp,
    sync_run::SyncRun,
};
use tokio::time::timeout;

use crate::engine::executor::{ExecutorCtx, execute};
use crate::engine::observability::{
    emit_cycle_finish, emit_cycle_start, emit_pair_errored, emit_phase_timing,
};
use crate::engine::planner::plan;
use crate::engine::reconcile::reconcile;
use crate::engine::types::{CycleSummary, Decision, EngineError};
use crate::limits::{CYCLE_PHASE_TIMEOUT_MS, MAX_QUEUE_DEPTH_PER_PAIR};
use crate::ports::{AuditSink, Clock, GraphError, IdGenerator, LocalFsError, StateStore};
use crate::ports::{LocalFs, RemoteDrive};

/// All external dependencies injected into `run_cycle`.
pub struct EngineDeps<'a, I: IdGenerator> {
    /// State store.
    pub state: &'a dyn StateStore,
    /// Local filesystem.
    pub local: &'a dyn LocalFs,
    /// Remote drive.
    pub remote: &'a dyn RemoteDrive,
    /// Clock.
    pub clock: &'a dyn Clock,
    /// Id generator.
    pub ids: &'a I,
    /// Audit sink.
    pub audit: &'a dyn AuditSink,
    /// Local hostname, used for conflict loser-rename naming.
    pub host: String,
}

/// Run one sync cycle for a pair.
///
/// # Errors
///
/// Returns an [`EngineError`] if any phase encounters a fatal error.
#[allow(clippy::too_many_lines)]
pub async fn run_cycle<I: IdGenerator>(
    deps: &EngineDeps<'_, I>,
    pair_id: PairId,
    trigger: RunTrigger,
) -> Result<CycleSummary, EngineError> {
    // ── Load pair ──────────────────────────────────────────────────────────
    let pair = deps
        .state
        .pair_get(&pair_id)
        .await?
        .ok_or_else(|| EngineError::PairNotRunnable(format!("pair {pair_id} not found")))?;

    if pair.paused || pair.status == PairStatus::Errored {
        return Err(EngineError::PairNotRunnable(format!(
            "pair {} is {:?} (paused={})",
            pair_id, pair.status, pair.paused
        )));
    }

    let started_at = deps.clock.now();
    let run_id = deps.ids.new_id::<SyncRunTag>();

    // Emit cycle.start.
    emit_cycle_start(
        deps.audit,
        deps.clock,
        deps.ids,
        pair_id,
        trigger_name(trigger),
    );

    // ── Phase 2: local scan delta (or full scan for initial sync) ──────────
    let phase2_start = std::time::Instant::now();
    let local_entries = run_phase("local_scan", CYCLE_PHASE_TIMEOUT_MS, async {
        collect_local_entries(deps, &pair, pair.delta_token.is_none()).await
    })
    .await?;
    emit_phase_timing(
        deps.audit,
        deps.clock,
        deps.ids,
        pair_id,
        "local_scan",
        u64::try_from(phase2_start.elapsed().as_millis()).unwrap_or(u64::MAX),
    );

    // ── Phase 3: remote scan delta ─────────────────────────────────────────
    let phase3_start = std::time::Instant::now();
    let (remote_entries, _new_cursor) = run_phase("remote_scan", CYCLE_PHASE_TIMEOUT_MS, async {
        collect_remote_entries(deps, &pair).await
    })
    .await?;
    emit_phase_timing(
        deps.audit,
        deps.clock,
        deps.ids,
        pair_id,
        "remote_scan",
        u64::try_from(phase3_start.elapsed().as_millis()).unwrap_or(u64::MAX),
    );

    // ── Phase 4: reconcile ─────────────────────────────────────────────────
    let phase4_start = std::time::Instant::now();
    let decisions = reconcile_all(deps, &pair, &local_entries, &remote_entries, started_at).await?;
    emit_phase_timing(
        deps.audit,
        deps.clock,
        deps.ids,
        pair_id,
        "reconcile",
        u64::try_from(phase4_start.elapsed().as_millis()).unwrap_or(u64::MAX),
    );

    // ── Phase 5: plan FileOps ──────────────────────────────────────────────
    let phase5_start = std::time::Instant::now();
    let op_plan = plan(decisions, pair_id, run_id, deps.clock, deps.ids);
    emit_phase_timing(
        deps.audit,
        deps.clock,
        deps.ids,
        pair_id,
        "plan",
        u64::try_from(phase5_start.elapsed().as_millis()).unwrap_or(u64::MAX),
    );

    // ── Phase 6: execute FileOps ───────────────────────────────────────────
    let phase6_start = std::time::Instant::now();
    let mut local_ops: u32 = 0;
    let mut remote_ops: u32 = 0;
    let bytes_uploaded: u64 = 0;
    let bytes_downloaded: u64 = 0;
    let mut had_failure = false;

    let executor_ctx = ExecutorCtx {
        store: deps.state,
        local: deps.local,
        remote: deps.remote,
    };

    for op in &op_plan.ops {
        deps.state.op_insert(op).await?;

        if let Err(e) = execute(&executor_ctx, op, &pair).await {
            had_failure = true;
            // Categorise non-recoverable errors and transition pair.
            if let Some(reason) = non_recoverable_reason(&e) {
                transition_pair_errored(deps, &pair, reason).await?;
                break;
            }
        } else {
            // Count ops by kind.
            use onesync_protocol::enums::FileOpKind;
            match op.kind {
                FileOpKind::Upload
                | FileOpKind::RemoteMkdir
                | FileOpKind::RemoteDelete
                | FileOpKind::RemoteRename => remote_ops += 1,
                FileOpKind::Download
                | FileOpKind::LocalMkdir
                | FileOpKind::LocalDelete
                | FileOpKind::LocalRename => local_ops += 1,
            }
        }
    }
    let _ = bytes_uploaded;
    let _ = bytes_downloaded;

    emit_phase_timing(
        deps.audit,
        deps.clock,
        deps.ids,
        pair_id,
        "execute",
        u64::try_from(phase6_start.elapsed().as_millis()).unwrap_or(u64::MAX),
    );

    // ── Phase 7: record SyncRun ────────────────────────────────────────────
    let finished_at = deps.clock.now();
    let outcome = if had_failure {
        RunOutcome::PartialFailure
    } else {
        RunOutcome::Success
    };

    let sync_run = SyncRun {
        id: run_id,
        pair_id,
        trigger,
        started_at,
        finished_at: Some(finished_at),
        local_ops,
        remote_ops,
        bytes_uploaded,
        bytes_downloaded,
        outcome: Some(outcome),
        outcome_detail: if op_plan.truncated {
            Some("planning truncated".into())
        } else {
            None
        },
    };
    deps.state.run_record(&sync_run).await?;

    emit_cycle_finish(
        deps.audit,
        deps.clock,
        deps.ids,
        pair_id,
        match outcome {
            RunOutcome::Success => "success",
            RunOutcome::PartialFailure => "partial_failure",
            RunOutcome::Aborted => "aborted",
        },
        local_ops,
        remote_ops,
    );

    Ok(CycleSummary {
        run_id,
        pair_id,
        trigger,
        started_at,
        finished_at,
        outcome,
        local_ops,
        remote_ops,
        bytes_uploaded,
        bytes_downloaded,
    })
}

/// Wrap an async block in a phase timeout.
async fn run_phase<F, T>(phase: &'static str, timeout_ms: u64, fut: F) -> Result<T, EngineError>
where
    F: std::future::Future<Output = Result<T, EngineError>>,
{
    timeout(Duration::from_millis(timeout_ms), fut)
        .await
        .map_err(|_| EngineError::PhaseTimeout { phase })?
}

/// Collect local path → `FileSide` snapshot from the state store.
/// On initial sync (no cursor), we scan from the local fs.
async fn collect_local_entries<I: IdGenerator>(
    deps: &EngineDeps<'_, I>,
    pair: &Pair,
    _full_scan: bool,
) -> Result<Vec<(RelPath, onesync_protocol::file_side::FileSide)>, EngineError> {
    // For delta cycles: return dirty entries from the state store.
    // For initial sync: scan from the local fs.
    // This simplified implementation reads dirty entries for both cases.
    let dirty = deps
        .state
        .file_entries_dirty(&pair.id, MAX_QUEUE_DEPTH_PER_PAIR)
        .await?;
    let entries: Vec<(RelPath, onesync_protocol::file_side::FileSide)> = dirty
        .into_iter()
        .filter_map(|e| e.local.map(|l| (e.relative_path, l)))
        .collect();
    Ok(entries)
}

/// Collect remote delta items. Returns (items, `new_cursor`).
async fn collect_remote_entries<I: IdGenerator>(
    deps: &EngineDeps<'_, I>,
    pair: &Pair,
) -> Result<
    (
        Vec<(RelPath, onesync_protocol::file_side::FileSide)>,
        Option<onesync_protocol::primitives::DeltaCursor>,
    ),
    EngineError,
> {
    // Call RemoteDrive::delta. The placeholder DeltaPage contains no items yet.
    let _page = deps
        .remote
        .delta(&pair.account_id_placeholder(), pair.delta_token.as_ref())
        .await
        .map_err(|e| match e {
            GraphError::ResyncRequired => {
                // Caller should handle by re-running as initial sync.
                EngineError::Graph(GraphError::ResyncRequired)
            }
            other => EngineError::Graph(other),
        })?;

    // DeltaPage is a placeholder unit struct; no items to process in this milestone.
    Ok((Vec::new(), None))
}

/// Run reconciliation over all affected paths.
async fn reconcile_all<I: IdGenerator>(
    deps: &EngineDeps<'_, I>,
    pair: &Pair,
    local_entries: &[(RelPath, onesync_protocol::file_side::FileSide)],
    remote_entries: &[(RelPath, onesync_protocol::file_side::FileSide)],
    detected_at: Timestamp,
) -> Result<Vec<(RelPath, Decision)>, EngineError> {
    // Collect all paths observed in either delta.
    let mut all_paths: BTreeSet<RelPath> = BTreeSet::new();
    for (p, _) in local_entries {
        all_paths.insert(p.clone());
    }
    for (p, _) in remote_entries {
        all_paths.insert(p.clone());
    }

    let existing: BTreeSet<RelPath> = all_paths.clone();
    let mut decisions = Vec::new();

    for path in all_paths {
        let local_side = local_entries
            .iter()
            .find(|(p, _)| p == &path)
            .map(|(_, s)| s);
        let remote_side = remote_entries
            .iter()
            .find(|(p, _)| p == &path)
            .map(|(_, s)| s);

        // Load the current file entry for the synced side.
        let synced_side = deps
            .state
            .file_entry_get(&pair.id, &path)
            .await?
            .as_ref()
            .and_then(|e| e.synced.clone());

        let decision = reconcile(
            &path,
            synced_side.as_ref(),
            local_side,
            remote_side,
            &deps.host,
            detected_at,
            &existing,
        );

        if decision != Decision::Clean {
            // Update FileEntry sync_state to reflect the decision.
            let updated_state = match &decision {
                Decision::UploadLocalToRemote => FileSyncState::PendingUpload,
                Decision::DownloadRemoteToLocal => FileSyncState::PendingDownload,
                Decision::Conflict { .. } => FileSyncState::PendingConflict,
                Decision::DeleteRemote | Decision::DeleteLocal => FileSyncState::Dirty,
                Decision::Clean => FileSyncState::Clean,
            };
            let entry = FileEntry {
                pair_id: pair.id,
                relative_path: path.clone(),
                kind: local_side
                    .or(remote_side)
                    .map_or(onesync_protocol::enums::FileKind::File, |s| s.kind),
                sync_state: updated_state,
                local: local_side.cloned(),
                remote: remote_side.cloned(),
                synced: synced_side,
                pending_op_id: None,
                updated_at: detected_at,
            };
            deps.state.file_entry_upsert(&entry).await?;
            decisions.push((path, decision));
        }
    }

    Ok(decisions)
}

/// Transition a pair to `Errored` and emit the `pair.errored` audit event.
async fn transition_pair_errored<I: IdGenerator>(
    deps: &EngineDeps<'_, I>,
    pair: &Pair,
    reason: &str,
) -> Result<(), EngineError> {
    let mut updated = pair.clone();
    updated.status = PairStatus::Errored;
    updated.errored_reason = Some(reason.to_owned());
    updated.updated_at = deps.clock.now();
    deps.state.pair_upsert(&updated).await?;
    emit_pair_errored(deps.audit, deps.clock, deps.ids, pair.id, reason);
    Ok(())
}

/// Map an `EngineError` to a non-recoverable error reason string, or `None` if retryable.
const fn non_recoverable_reason(e: &EngineError) -> Option<&'static str> {
    match e {
        EngineError::Graph(GraphError::Unauthorized | GraphError::ReAuthRequired) => Some("auth"),
        EngineError::LocalFs(LocalFsError::NotMounted(_)) => Some("local-missing"),
        EngineError::Graph(GraphError::NotFound) => Some("remote-missing"),
        EngineError::LocalFs(LocalFsError::PermissionDenied(_)) => Some("permission"),
        _ => None,
    }
}

/// Human-readable name for a trigger.
const fn trigger_name(t: RunTrigger) -> &'static str {
    match t {
        RunTrigger::Scheduled => "scheduled",
        RunTrigger::LocalEvent => "local_event",
        RunTrigger::RemoteWebhook => "remote_webhook",
        RunTrigger::CliForce => "cli_force",
        RunTrigger::BackoffRetry => "backoff_retry",
    }
}

// ─── Extension trait to work around the placeholder DriveId on Pair ───────────

trait PairExt {
    fn account_id_placeholder(&self) -> onesync_protocol::primitives::DriveId;
}

impl PairExt for Pair {
    fn account_id_placeholder(&self) -> onesync_protocol::primitives::DriveId {
        // In production this comes from the Account record.
        // Placeholder: use the remote_item_id as a stand-in drive id.
        onesync_protocol::primitives::DriveId::new(self.remote_item_id.as_str().to_owned())
    }
}
