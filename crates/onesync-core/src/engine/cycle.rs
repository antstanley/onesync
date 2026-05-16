//! `run_cycle`: the six-phase sync-cycle driver.
//!
//! Phases:
//! 1. **Delta** — fetch remote changes since the last cursor.
//! 2. **Local scan** — walk the pair root for new/diverged local files.
//! 3. **Reconcile** — pure: for each changed path produce a [`Decision`].
//! 4. **Plan** — convert decisions into ordered [`FileOp`]s.
//! 5. **Execute** — drive each op through the port layer.
//! 6. **Record** — persist the `SyncRun` and emit audit events.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use onesync_protocol::{
    audit::AuditEvent,
    conflict::Conflict,
    enums::{
        AuditLevel, ConflictSide, FileKind, FileOpStatus, FileSyncState, RunOutcome, RunTrigger,
    },
    file_entry::FileEntry,
    file_op::FileOp,
    file_side::FileSide,
    id::{AuditEventId, AuditTag, ConflictTag, SyncRunId},
    path::{AbsPath, RelPath},
    primitives::DeltaCursor,
    remote::RemoteItem,
    sync_run::SyncRun,
};

use crate::{
    engine::{
        case_collision::{case_collision_rename_target, case_folds_equal},
        executor::{execute, is_retriable},
        observability::{cycle_finished, cycle_started, op_failed},
        planner::plan,
        reconcile::reconcile_one,
        retry::{RetryDecision, retry_decision},
        types::{CycleSummary, Decision, DecisionKind, EngineError},
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

    // Phase 1 + 3a: delta → reconcile → decisions.
    let DeltaReconcileOutcome {
        mut decisions,
        mut conflicts_detected,
        remote_items_seen,
        delta_token,
        remote_items_by_path,
    } = phase_delta_reconcile(ctx).await?;

    // RP1-F10: advance the pair's persisted delta cursor after every delta-page
    // item has been written into `FileEntry.remote` by `phase_delta_reconcile`.
    // Per `docs/spec/03-sync-engine.md` lines 102-105 the cursor must not move
    // before per-item persistence completes. If the Pair row is absent (legacy
    // tests, in-flight init) we silently skip — the cycle is still valid as a
    // decision computer.
    if let Some(mut pair) = ctx
        .state
        .pair_get(&ctx.pair_id)
        .await
        .map_err(|e| EngineError::Port(e.to_string()))?
        && pair.delta_token != delta_token
    {
        pair.delta_token.clone_from(&delta_token);
        pair.updated_at = ctx.clock.now();
        ctx.state
            .pair_upsert(&pair)
            .await
            .map_err(|e| EngineError::Port(e.to_string()))?;
    }

    // Phase 2 + 3b: local scan → upload decisions for untracked / diverged paths, plus
    // case-collision detection that renames the local loser and records a Conflict row.
    let remote_paths: HashSet<RelPath> =
        decisions.iter().map(|d| d.relative_path.clone()).collect();
    let LocalUploadOutcome {
        decisions: local_decisions,
        local_events_seen,
        collisions_recorded,
    } = phase_local_uploads(ctx, &remote_paths, &remote_items_by_path).await?;
    decisions.extend(local_decisions);
    conflicts_detected += collisions_recorded;

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
        local_events_seen,
        ops_applied,
        conflicts_detected,
        delta_token,
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

/// Output of [`phase_delta_reconcile`].
struct DeltaReconcileOutcome {
    decisions: Vec<crate::engine::types::Decision>,
    conflicts_detected: usize,
    remote_items_seen: usize,
    delta_token: Option<DeltaCursor>,
    /// Map of remote paths (live, non-tombstoned only) to their `RemoteItem`. Used by
    /// the local-upload phase for case-collision detection — we need the remote `FileSide`
    /// to record a faithful `Conflict` row.
    remote_items_by_path: HashMap<RelPath, RemoteItem>,
}

/// Phase 1–3: fetch delta, reconcile each item, return the outcome shape above.
async fn phase_delta_reconcile<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
) -> Result<DeltaReconcileOutcome, EngineError> {
    let delta_page = ctx
        .remote
        .delta(&ctx.drive_id, ctx.cursor.as_ref())
        .await
        .map_err(|e| EngineError::Port(e.to_string()))?;

    let remote_items_seen = delta_page.items.len();
    let delta_token = delta_page.delta_token.clone();
    let mut decisions = Vec::new();
    let mut conflicts_detected = 0usize;
    let mut remote_items_by_path: HashMap<RelPath, RemoteItem> = HashMap::new();

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
            remote_items_by_path.insert(rel_path.clone(), remote_item.clone());
            Some(remote_item)
        };

        let now = ctx.clock.now();
        let new_remote = remote_opt.map(|item| remote_side_from_item(item, now));

        // RP1-F10: persist this page's remote-side observation into
        // `FileEntry.remote` so it survives across the cursor advance
        // (spec `03-sync-engine.md` lines 102-105). Tombstones for paths we
        // never tracked are no-ops — nothing to merge.
        if let Some(persisted) = build_remote_observation(
            ctx.pair_id,
            &rel_path,
            entry.as_ref(),
            remote_opt,
            new_remote,
            now,
        ) {
            ctx.state
                .file_entry_upsert(&persisted)
                .await
                .map_err(|e| EngineError::Port(e.to_string()))?;
        }

        let decision = reconcile_one(ctx.pair_id, rel_path, entry.as_ref(), remote_opt);

        if matches!(decision.kind, DecisionKind::Conflict { .. }) {
            conflicts_detected += 1;
        }

        decisions.push(decision);
    }

    Ok(DeltaReconcileOutcome {
        decisions,
        conflicts_detected,
        remote_items_seen,
        delta_token,
        remote_items_by_path,
    })
}

/// Output of [`phase_local_uploads`].
struct LocalUploadOutcome {
    decisions: Vec<Decision>,
    local_events_seen: usize,
    collisions_recorded: usize,
}

/// Phase 2 + 3b: scan the local root, detect untracked or diverged files, emit
/// `Upload`/`RemoteMkdir` decisions for them, and resolve case-collisions by renaming
/// the local-side loser.
///
/// Paths already covered by remote-delta decisions are skipped (the remote side wins
/// for the first cycle's reconciliation). For paths whose name case-folds-equal to a
/// remote path but isn't byte-identical, the local file is renamed using
/// `case_collision_rename_target` and a `Conflict` row is recorded. The renamed file
/// then enters the upload pipeline on the next cycle as a fresh untracked path.
async fn phase_local_uploads<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
    already_decided: &HashSet<RelPath>,
    remote_items_by_path: &HashMap<RelPath, RemoteItem>,
) -> Result<LocalUploadOutcome, EngineError> {
    let scan = ctx
        .local
        .scan(&ctx.local_root)
        .await
        .map_err(|e| EngineError::Port(e.to_string()))?;

    let mut decisions = Vec::new();
    let mut local_events_seen = 0usize;
    let mut collisions_recorded = 0usize;

    for (abs_path, side) in scan.entries {
        let Some(rel_path) = rel_from_abs(&ctx.local_root, &abs_path) else {
            continue;
        };
        if already_decided.contains(&rel_path) {
            continue;
        }
        local_events_seen += 1;

        // Case-collision check: another remote path differs only in case. Rename the
        // local file and record a Conflict instead of emitting an Upload.
        if let Some(remote_path) = find_case_collision(&rel_path, remote_items_by_path) {
            handle_case_collision(ctx, &rel_path, &side, remote_path, remote_items_by_path).await?;
            collisions_recorded += 1;
            continue;
        }

        let entry = ctx
            .state
            .file_entry_get(&ctx.pair_id, &rel_path)
            .await
            .map_err(|e| EngineError::Port(e.to_string()))?;

        let needs_upload = entry.as_ref().is_none_or(|e| match e.sync_state {
            FileSyncState::InFlight | FileSyncState::PendingConflict => false,
            _ => local_diverges_from_synced(&side, e.synced.as_ref()),
        });
        if !needs_upload {
            continue;
        }

        let new_entry = FileEntry {
            pair_id: ctx.pair_id,
            relative_path: rel_path.clone(),
            kind: side.kind,
            sync_state: FileSyncState::PendingUpload,
            local: Some(side.clone()),
            remote: entry.as_ref().and_then(|e| e.remote.clone()),
            synced: entry.as_ref().and_then(|e| e.synced.clone()),
            pending_op_id: entry.as_ref().and_then(|e| e.pending_op_id),
            updated_at: ctx.clock.now(),
        };
        ctx.state
            .file_entry_upsert(&new_entry)
            .await
            .map_err(|e| EngineError::Port(e.to_string()))?;

        let kind = if side.kind == FileKind::Directory {
            DecisionKind::RemoteMkdir
        } else {
            DecisionKind::Upload
        };
        decisions.push(Decision {
            pair_id: ctx.pair_id,
            relative_path: rel_path,
            kind,
        });
    }

    Ok(LocalUploadOutcome {
        decisions,
        local_events_seen,
        collisions_recorded,
    })
}

/// Return the remote path that case-folds equal to `local` but differs in byte form, or
/// `None` if no collision is present.
fn find_case_collision<'a>(
    local: &RelPath,
    remote_items_by_path: &'a HashMap<RelPath, RemoteItem>,
) -> Option<&'a RelPath> {
    remote_items_by_path
        .keys()
        .find(|remote| *remote != local && case_folds_equal(remote, local))
}

/// Rename the local-side loser, persist a `Conflict` row, and emit an audit event.
async fn handle_case_collision<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
    local_rel: &RelPath,
    local_side: &FileSide,
    remote_rel: &RelPath,
    remote_items_by_path: &HashMap<RelPath, RemoteItem>,
) -> Result<(), EngineError> {
    let now = ctx.clock.now();
    let rename_target_str = case_collision_rename_target(local_rel);
    let Ok(loser_rel) = rename_target_str.parse::<RelPath>() else {
        return Err(EngineError::Port(format!(
            "case-collision rename produced an invalid path: {rename_target_str}"
        )));
    };
    let Some(from_abs) = join_abs(&ctx.local_root, local_rel) else {
        return Err(EngineError::Port(format!(
            "cannot join local_root with {local_rel}"
        )));
    };
    let Some(to_abs) = join_abs(&ctx.local_root, &loser_rel) else {
        return Err(EngineError::Port(format!(
            "cannot join local_root with {loser_rel}"
        )));
    };
    ctx.local
        .rename(&from_abs, &to_abs)
        .await
        .map_err(|e| EngineError::Port(e.to_string()))?;

    let remote_side = remote_items_by_path
        .get(remote_rel)
        .map_or_else(synthetic_remote_side, |item| {
            remote_side_from_item(item, now)
        });

    let conflict = Conflict {
        id: ctx.ids.new_id::<ConflictTag>(),
        pair_id: ctx.pair_id,
        relative_path: remote_rel.clone(),
        winner: ConflictSide::Remote,
        loser_relative_path: loser_rel,
        local_side: local_side.clone(),
        remote_side,
        detected_at: now,
        resolved_at: None,
        resolution: None,
        note: None,
    };
    ctx.state
        .conflict_insert(&conflict)
        .await
        .map_err(|e| EngineError::Port(e.to_string()))?;

    let evt = AuditEvent {
        id: ctx.ids.new_id::<AuditTag>(),
        ts: now,
        level: AuditLevel::Warn,
        kind: "local.case_collision.renamed".to_owned(),
        pair_id: Some(ctx.pair_id),
        payload: {
            let mut m = serde_json::Map::new();
            m.insert(
                "from".to_owned(),
                serde_json::Value::String(local_rel.as_str().to_owned()),
            );
            m.insert(
                "to".to_owned(),
                serde_json::Value::String(conflict.loser_relative_path.as_str().to_owned()),
            );
            m.insert(
                "remote_path".to_owned(),
                serde_json::Value::String(remote_rel.as_str().to_owned()),
            );
            m
        },
    };
    ctx.audit.emit(evt);

    Ok(())
}

fn join_abs(root: &AbsPath, rel: &RelPath) -> Option<AbsPath> {
    format!("{}/{}", root.as_str(), rel.as_str()).parse().ok()
}

/// Build the `FileEntry` shape to persist for one delta-page observation.
///
/// Returns `None` only when the item is a tombstone for a path we never
/// tracked — there is no existing row to merge into and no live remote-side
/// data to record, so no upsert is required.
///
/// When an entry already exists, the only field touched is `remote` (and
/// `updated_at`); the rest of the row — `local`, `synced`, `sync_state`,
/// `pending_op_id` — is preserved so concurrent local-side work isn't
/// clobbered. When no entry exists and the item is live, a fresh row is
/// created in `PendingDownload` state per spec's initial-sync rule.
fn build_remote_observation(
    pair_id: onesync_protocol::id::PairId,
    rel_path: &RelPath,
    existing: Option<&FileEntry>,
    remote_opt: Option<&RemoteItem>,
    new_remote: Option<FileSide>,
    now: onesync_protocol::primitives::Timestamp,
) -> Option<FileEntry> {
    if let Some(existing) = existing {
        return Some(FileEntry {
            remote: new_remote,
            updated_at: now,
            ..existing.clone()
        });
    }
    let item = remote_opt?;
    Some(FileEntry {
        pair_id,
        relative_path: rel_path.clone(),
        kind: if item.is_folder() {
            FileKind::Directory
        } else {
            FileKind::File
        },
        sync_state: FileSyncState::PendingDownload,
        local: None,
        remote: new_remote,
        synced: None,
        pending_op_id: None,
        updated_at: now,
    })
}

fn remote_side_from_item(
    item: &RemoteItem,
    now: onesync_protocol::primitives::Timestamp,
) -> FileSide {
    FileSide {
        kind: if item.is_folder() {
            FileKind::Directory
        } else {
            FileKind::File
        },
        size_bytes: item.size,
        content_hash: None,
        mtime: now,
        etag: item
            .e_tag
            .as_deref()
            .map(onesync_protocol::primitives::ETag::new),
        remote_item_id: Some(onesync_protocol::primitives::DriveItemId::new(
            item.id.clone(),
        )),
    }
}

fn synthetic_remote_side() -> FileSide {
    FileSide {
        kind: FileKind::File,
        size_bytes: 0,
        content_hash: None,
        mtime: onesync_protocol::primitives::Timestamp::from_datetime(
            chrono::DateTime::from_timestamp(0, 0).unwrap_or_default(),
        ),
        etag: None,
        remote_item_id: None,
    }
}

/// Return the `RelPath` that `abs` represents under `root`, or `None` when:
/// * `abs` is not a child of `root`,
/// * the relative portion is empty (i.e. `abs == root`),
/// * or the relative string fails `RelPath` validation.
fn rel_from_abs(root: &AbsPath, abs: &Path) -> Option<RelPath> {
    let abs_str = abs.to_str()?;
    let rel = abs_str.strip_prefix(root.as_str())?.trim_start_matches('/');
    if rel.is_empty() {
        return None;
    }
    rel.parse().ok()
}

/// Heuristic local-vs-synced divergence check.
///
/// Directory pairs are treated as identical (folders have no content). For files we
/// compare `size_bytes` then `mtime`; either mismatch forces an upload. We deliberately
/// avoid hashing here — the scan does not populate `content_hash` and `LocalFs::hash` is
/// expensive on every cycle. False positives are acceptable; false negatives (skipping
/// a real edit) would be a correctness bug.
fn local_diverges_from_synced(side: &FileSide, synced: Option<&FileSide>) -> bool {
    let Some(synced) = synced else {
        return true;
    };
    if side.kind == FileKind::Directory && synced.kind == FileKind::Directory {
        return false;
    }
    if side.size_bytes != synced.size_bytes {
        return true;
    }
    side.mtime != synced.mtime
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
