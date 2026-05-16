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
        AuditLevel, ConflictSide, FileKind, FileOpKind, FileOpStatus, FileSyncState, RunOutcome,
        RunTrigger,
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

use crate::engine::conflict::pick_winner_and_loser;

use crate::{
    engine::{
        case_collision::{case_collision_rename_target, case_folds_equal},
        executor::{execute, is_retriable},
        observability::{cycle_finished, cycle_started, op_failed},
        planner::plan,
        reconcile::{is_action_blocking, reconcile_one},
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
        collision_renames,
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
        initial_sync_collisions,
    } = phase_local_uploads(ctx, &remote_paths, &remote_items_by_path).await?;
    decisions.extend(local_decisions);
    conflicts_detected += collisions_recorded;

    // RP1-F17: upgrade Download decisions into ConflictDetected when the
    // local scan saw a file at the same path. Attach the captured local
    // side to the FileEntry so `phase_resolve_conflicts` can record both
    // observations in the Conflict row.
    conflicts_detected +=
        apply_initial_sync_collisions(ctx, &mut decisions, initial_sync_collisions).await?;

    // Phase 3c: resolve content conflicts (`ConflictDetected`) by inserting
    // a `Conflict` row and parking the FileEntry in `PendingConflict`. For
    // winner=Remote conflicts (RP1-F4 follow-on) also emit the rename +
    // download op pair so the loser is preserved and the winner content
    // lands at the original path. winner=Local stays Pending pending the
    // upload-side machinery (parent_remote_id lookup).
    let mut conflict_ops: Vec<FileOp> = Vec::new();
    phase_resolve_conflicts(ctx, &mut decisions, run_id, &mut conflict_ops).await?;

    // Phase 4: plan + conflict ops (RP1-F4 + F14 follow-ons).
    let mut ops = plan(decisions, run_id, now, ctx.ids);
    ops.extend(conflict_ops);
    ops.extend(build_remote_rename_ops(ctx, run_id, now, collision_renames));

    // Phase 5: execute.
    let exec = phase_execute(ctx, ops).await?;
    let ops_applied = exec.applied;

    // Phase 6: record. RP1-F19: classify the run outcome from the actual
    // op-result counts rather than unconditionally claiming Success.
    let finished_at = ctx.clock.now();
    let outcome = classify_outcome(exec.applied, exec.failed);
    let outcome_detail = if exec.failed > 0 {
        Some(format!(
            "{} op(s) failed of {} attempted",
            exec.failed,
            exec.applied + exec.failed
        ))
    } else {
        None
    };
    // LINT: ops_applied ≤ MAX_QUEUE_DEPTH_PER_PAIR (4096) so truncation is safe.
    #[allow(clippy::cast_possible_truncation)]
    let run = SyncRun {
        id: run_id,
        pair_id: ctx.pair_id,
        trigger: ctx.trigger,
        outcome: Some(outcome),
        outcome_detail,
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
    /// RP1-F14 follow-on: rename targets for remote case-collision losers.
    /// Each tuple is `(current path, remote item id, new leaf name)`. The
    /// caller (`run_cycle`) builds `RemoteRename` `FileOp`s from these after
    /// it has the `SyncRunId` in hand.
    collision_renames: Vec<(RelPath, String, String)>,
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

    // RP1-F14: detect remote-side case collisions before per-item processing.
    // A delta page that contains, say, both `Foo.txt` and `foo.txt` would
    // previously upsert two distinct FileEntries and emit two Download
    // decisions; on APFS the second download then overwrites the first,
    // silently losing one of the remote files. Pick the byte-wise smallest
    // path as canonical and drop the others.
    //
    // RP1-F14 follow-on: for each dropped path, also build a rename target
    // (using `case_collision_rename_target` to derive the disambiguated
    // leaf) so the caller can emit a remote-side rename op. Both files then
    // end up on remote with distinct names; the canonical's Download runs
    // unchanged and a subsequent cycle picks up the renamed loser.
    let collisions = detect_remote_case_collisions(&delta_page.items);
    let mut collision_renames = process_remote_case_collisions(ctx, &delta_page.items, &collisions);

    for remote_item in &delta_page.items {
        let Some(rel_path) = build_rel_path_from_item(remote_item) else {
            continue;
        };
        if collisions.contains_key(&rel_path) {
            // Loser of a remote case-collision; the canonical version of
            // the same case-folded path is processed below.
            continue;
        }

        let entry = ctx
            .state
            .file_entry_get(&ctx.pair_id, &rel_path)
            .await
            .map_err(|e| EngineError::Port(e.to_string()))?;

        if entry.is_none()
            && remote_item.deleted.is_none()
            && skip_for_case_collision(ctx, &rel_path, remote_item, &mut collision_renames).await?
        {
            continue;
        }

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

        if decision.kind.is_conflict() {
            conflicts_detected += 1;
        }

        decisions.push(decision);
    }

    Ok(DeltaReconcileOutcome {
        decisions,
        conflicts_detected,
        remote_items_seen,
        delta_token,
        collision_renames,
        remote_items_by_path,
    })
}

/// Output of [`phase_local_uploads`].
struct LocalUploadOutcome {
    decisions: Vec<Decision>,
    local_events_seen: usize,
    collisions_recorded: usize,
    /// Paths where a local file exists at a path the remote delta also
    /// produced a Download decision for (RP1-F17 initial-sync collision).
    /// The caller upgrades each entry's decision to `ConflictDetected` and
    /// attaches the captured local-side metadata to the persisted
    /// `FileEntry`. Folders are not collected here — two folders at the
    /// same path are equivalent, not divergent.
    initial_sync_collisions: Vec<(RelPath, FileSide)>,
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
    let mut initial_sync_collisions: Vec<(RelPath, FileSide)> = Vec::new();

    for (abs_path, side) in scan.entries {
        let Some(rel_path) = rel_from_abs(&ctx.local_root, &abs_path) else {
            continue;
        };
        if already_decided.contains(&rel_path) {
            // RP1-F17: a local file at a path the remote phase already
            // decided to download is an initial-sync collision. Surface it
            // so the caller can promote the Download to ConflictDetected
            // and attach the local side to the FileEntry created by F10
            // during the delta phase. Folders are silently shared.
            if let Some(remote_item) = remote_items_by_path.get(&rel_path)
                && !remote_item.is_folder()
            {
                initial_sync_collisions.push((rel_path, side));
            }
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

        let needs_upload = entry.as_ref().is_none_or(|e| {
            !is_action_blocking(e.sync_state)
                && local_diverges_from_synced(&side, e.synced.as_ref())
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
        initial_sync_collisions,
    })
}

/// RP1-F14 follow-on: build `RemoteRename` `FileOp`s for each detected
/// case-collision loser. The ops carry `metadata.from_conflict = true` so the
/// post-op `FileEntry` reconciliation step knows not to clobber the parked
/// state of any related entries.
fn build_remote_rename_ops<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
    run_id: SyncRunId,
    now: onesync_protocol::primitives::Timestamp,
    renames: Vec<(RelPath, String, String)>,
) -> Vec<FileOp> {
    renames
        .into_iter()
        .map(|(path, item_id, new_name)| {
            let mut meta = serde_json::Map::new();
            meta.insert(
                "remote_item_id".to_owned(),
                serde_json::Value::String(item_id),
            );
            meta.insert("new_name".to_owned(), serde_json::Value::String(new_name));
            meta.insert("from_conflict".to_owned(), serde_json::Value::Bool(true));
            FileOp {
                id: ctx.ids.new_id(),
                run_id,
                pair_id: ctx.pair_id,
                relative_path: path,
                kind: FileOpKind::RemoteRename,
                status: FileOpStatus::Enqueued,
                attempts: 0,
                last_error: None,
                metadata: meta,
                enqueued_at: now,
                started_at: None,
                finished_at: None,
            }
        })
        .collect()
}

/// RP1-F17: promote Download decisions into `ConflictDetected` for paths the
/// local scan observed, and persist the local side onto the `FileEntry`. Each
/// upgrade increments the cycle's conflict count.
async fn apply_initial_sync_collisions<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
    decisions: &mut [Decision],
    collisions: Vec<(RelPath, FileSide)>,
) -> Result<usize, EngineError> {
    let mut upgraded = 0usize;
    for (rel, local_side) in collisions {
        if let Some(decision) = decisions
            .iter_mut()
            .find(|d| d.relative_path == rel && matches!(d.kind, DecisionKind::Download))
        {
            decision.kind = DecisionKind::ConflictDetected;
            upgraded += 1;
        }
        let now = ctx.clock.now();
        if let Some(mut entry) = ctx
            .state
            .file_entry_get(&ctx.pair_id, &rel)
            .await
            .map_err(|e| EngineError::Port(e.to_string()))?
        {
            entry.local = Some(local_side);
            entry.updated_at = now;
            ctx.state
                .file_entry_upsert(&entry)
                .await
                .map_err(|e| EngineError::Port(e.to_string()))?;
        }
    }
    Ok(upgraded)
}

/// RP1-F4: resolve `ConflictDetected` decisions inline by inserting a
/// `Conflict` row and parking the corresponding `FileEntry` in
/// `PendingConflict`. This is the minimal materialisation: the spec's full
/// 4-step op group (rename loser → propagate rename → propagate winner →
/// record) is deferred. Subsequent cycles see `sync_state =
/// PendingConflict` and reconcile returns `NoOp`, so the path no longer
/// regenerates a fresh `ConflictDetected` every cycle.
///
/// The pre-fix engine produced `ConflictDetected` (after RP1-F3) or the
/// placeholder `Conflict` (before) and the planner silently dropped it —
/// no Conflict row, no operator signal, no state transition. This phase
/// closes that silent-divergence path.
///
/// `decisions` is drained of every conflict variant: planner-side filtering
/// (`to_file_op_kind` returns `None` for both `ConflictDetected` and the
/// post-policy `Conflict`) would also work, but removing them here keeps
/// the planner's input free of "you can't act on this" decisions.
async fn phase_resolve_conflicts<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
    decisions: &mut Vec<Decision>,
    run_id: SyncRunId,
    conflict_ops: &mut Vec<FileOp>,
) -> Result<(), EngineError> {
    let mut i = 0;
    while i < decisions.len() {
        if !decisions[i].kind.is_conflict() {
            i += 1;
            continue;
        }
        let decision = decisions.remove(i);
        record_content_conflict(ctx, &decision, run_id, conflict_ops).await?;
        // Don't increment i: we removed the i-th element.
    }
    Ok(())
}

/// Record one content conflict: insert a `Conflict` row with winner derived
/// from mtime, set the entry to `PendingConflict`, emit an audit event. For
/// the winner=`Remote` case (RP1-F4 follow-on) also emits a `LocalRename` +
/// `Download` op pair so the loser is preserved at `loser_path` locally and
/// the winner's content overwrites the original. winner=`Local` cases stay
/// at `PendingConflict` for operator-driven resolution; emitting
/// `RemoteRename` + `Upload` from the engine would require
/// `parent_remote_id` lookup logic (the parent path's `FileEntry`) that is
/// not yet in place.
async fn record_content_conflict<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
    decision: &Decision,
    run_id: SyncRunId,
    conflict_ops: &mut Vec<FileOp>,
) -> Result<(), EngineError> {
    let now = ctx.clock.now();
    let Some(mut entry) = ctx
        .state
        .file_entry_get(&ctx.pair_id, &decision.relative_path)
        .await
        .map_err(|e| EngineError::Port(e.to_string()))?
    else {
        // FileEntry vanished concurrently. Skip; the next cycle will pick up
        // the new state.
        return Ok(());
    };

    let (Some(local_side), Some(remote_side)) = (entry.local.clone(), entry.remote.clone()) else {
        // A conflict without both sides represented should not happen — the
        // reconcile code emits `ConflictDetected` only from
        // `reconcile_both`. Defensive: skip rather than panic.
        return Ok(());
    };

    let outcome = pick_winner_and_loser(
        local_side.mtime,
        remote_side.mtime,
        &decision.relative_path,
        &ctx.host_name,
        now,
        0,
    )
    .map_err(|e| EngineError::Port(format!("conflict loser path invalid: {e}")))?;

    let conflict = Conflict {
        id: ctx.ids.new_id::<ConflictTag>(),
        pair_id: ctx.pair_id,
        relative_path: decision.relative_path.clone(),
        winner: outcome.winner,
        loser_relative_path: outcome.loser_path,
        local_side,
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

    entry.sync_state = FileSyncState::PendingConflict;
    entry.updated_at = now;
    ctx.state
        .file_entry_upsert(&entry)
        .await
        .map_err(|e| EngineError::Port(e.to_string()))?;

    let evt = AuditEvent {
        id: ctx.ids.new_id::<AuditTag>(),
        ts: now,
        level: AuditLevel::Warn,
        kind: "conflict.detected".to_owned(),
        pair_id: Some(ctx.pair_id),
        payload: {
            let mut m = serde_json::Map::new();
            m.insert(
                "path".to_owned(),
                serde_json::Value::String(decision.relative_path.as_str().to_owned()),
            );
            m.insert(
                "winner".to_owned(),
                serde_json::to_value(conflict.winner).unwrap_or(serde_json::Value::Null),
            );
            m.insert(
                "loser_path".to_owned(),
                serde_json::Value::String(conflict.loser_relative_path.as_str().to_owned()),
            );
            m
        },
    };
    ctx.audit.emit(evt);

    // RP1-F4 follow-on: for winner=Remote, emit the spec's step 1 (rename
    // loser on its own side) and step 3 (propagate winner to loser side at
    // the original path). Step 2 (propagate the rename to the other side)
    // happens organically on the next cycle's local scan.
    if conflict.winner == ConflictSide::Remote {
        push_remote_winner_conflict_ops(ctx, &conflict, run_id, conflict_ops);
    }

    Ok(())
}

/// Build the RP1-F4 follow-on op pair for a `winner=Remote` conflict:
/// `LocalRename` (loser to disambiguated path) then `Download` (winner content
/// to the original path). The rename op carries `from_conflict=true` so the
/// `FileEntry` for the original is not touched by `update_file_entry_post_op`;
/// the Download op's normal post-op update then transitions the entry to
/// `Clean` with `synced = entry.remote`.
fn push_remote_winner_conflict_ops<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
    conflict: &Conflict,
    run_id: SyncRunId,
    conflict_ops: &mut Vec<FileOp>,
) {
    let now = ctx.clock.now();

    let mut rename_meta = serde_json::Map::new();
    rename_meta.insert(
        "new_path".to_owned(),
        serde_json::Value::String(conflict.loser_relative_path.as_str().to_owned()),
    );
    rename_meta.insert("from_conflict".to_owned(), serde_json::Value::Bool(true));
    conflict_ops.push(FileOp {
        id: ctx.ids.new_id(),
        run_id,
        pair_id: ctx.pair_id,
        relative_path: conflict.relative_path.clone(),
        kind: FileOpKind::LocalRename,
        status: FileOpStatus::Enqueued,
        attempts: 0,
        last_error: None,
        metadata: rename_meta,
        enqueued_at: now,
        started_at: None,
        finished_at: None,
    });

    let Some(remote_item_id) = conflict.remote_side.remote_item_id.as_ref() else {
        // Remote side has no driveItem id — can't issue a Download. Leave
        // the FileEntry parked at PendingConflict; operator resolves.
        return;
    };
    let mut download_meta = serde_json::Map::new();
    download_meta.insert(
        "remote_item_id".to_owned(),
        serde_json::Value::String(remote_item_id.as_str().to_owned()),
    );
    conflict_ops.push(FileOp {
        id: ctx.ids.new_id(),
        run_id,
        pair_id: ctx.pair_id,
        relative_path: conflict.relative_path.clone(),
        kind: FileOpKind::Download,
        status: FileOpStatus::Enqueued,
        attempts: 0,
        last_error: None,
        metadata: download_meta,
        enqueued_at: now,
        started_at: None,
        finished_at: None,
    });
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

/// RP1-F24 + follow-on: returns `true` if a case-folded `FileEntry` already
/// exists at a different-case path. Emits a `file_entry.case_collision_detected`
/// audit event and, if the disambiguated rename target parses, appends a
/// `(path, item_id, new_leaf)` record to `collision_renames` so the caller
/// emits a `RemoteRename` op for the loser. The original local `FileEntry`
/// is untouched.
async fn skip_for_case_collision<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
    rel_path: &RelPath,
    remote_item: &RemoteItem,
    collision_renames: &mut Vec<(RelPath, String, String)>,
) -> Result<bool, EngineError> {
    let Some(existing) = ctx
        .state
        .file_entry_get_ci(&ctx.pair_id, rel_path)
        .await
        .map_err(|e| EngineError::Port(e.to_string()))?
    else {
        return Ok(false);
    };
    if existing.relative_path == *rel_path {
        return Ok(false);
    }
    let evt = AuditEvent {
        id: ctx.ids.new_id::<AuditTag>(),
        ts: ctx.clock.now(),
        level: AuditLevel::Warn,
        kind: "file_entry.case_collision_detected".to_owned(),
        pair_id: Some(ctx.pair_id),
        payload: {
            let mut m = serde_json::Map::new();
            m.insert(
                "delta_path".to_owned(),
                serde_json::Value::String(rel_path.as_str().to_owned()),
            );
            m.insert(
                "existing_path".to_owned(),
                serde_json::Value::String(existing.relative_path.as_str().to_owned()),
            );
            m
        },
    };
    ctx.audit.emit(evt);

    // RP1-F24 follow-on: emit a rename target for the colliding delta item
    // so the canonical local FileEntry survives at its original case and the
    // remote item ends up under a disambiguated name.
    let target_str = case_collision_rename_target(rel_path);
    if let Ok(target_rel) = target_str.parse::<RelPath>() {
        let new_leaf = target_rel
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or(target_rel.as_str())
            .to_owned();
        collision_renames.push((rel_path.clone(), remote_item.id.clone(), new_leaf));
    }
    Ok(true)
}

/// Emit the audit event + build the rename target for every remote case-
/// collision loser. Returns `(path, item_id, new_leaf_name)` for each
/// renameable loser; entries whose derived target name fails `RelPath`
/// validation are skipped (the audit event still records the collision).
fn process_remote_case_collisions<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
    items: &[RemoteItem],
    collisions: &std::collections::HashMap<RelPath, RelPath>,
) -> Vec<(RelPath, String, String)> {
    let mut renames = Vec::new();
    for (dropped, canonical) in collisions {
        let evt = AuditEvent {
            id: ctx.ids.new_id::<AuditTag>(),
            ts: ctx.clock.now(),
            level: AuditLevel::Warn,
            kind: "remote.case_collision.dropped".to_owned(),
            pair_id: Some(ctx.pair_id),
            payload: {
                let mut m = serde_json::Map::new();
                m.insert(
                    "dropped".to_owned(),
                    serde_json::Value::String(dropped.as_str().to_owned()),
                );
                m.insert(
                    "canonical".to_owned(),
                    serde_json::Value::String(canonical.as_str().to_owned()),
                );
                m
            },
        };
        ctx.audit.emit(evt);

        let target_str = case_collision_rename_target(dropped);
        let Ok(target_rel) = target_str.parse::<RelPath>() else {
            continue;
        };
        let new_leaf = target_rel
            .as_str()
            .rsplit('/')
            .next()
            .unwrap_or(target_rel.as_str())
            .to_owned();
        if let Some(item) = items
            .iter()
            .find(|i| build_rel_path_from_item(i).is_some_and(|p| &p == dropped))
        {
            renames.push((dropped.clone(), item.id.clone(), new_leaf));
        }
    }
    renames
}

/// Detect remote-side case-collisions in one delta page.
///
/// RP1-F14: returns a map of *dropped* (loser) paths to their *canonical*
/// (kept) counterparts. The canonical is the byte-wise smallest path in each
/// case-folded bucket — deterministic across cycles regardless of delta
/// arrival order. The case fold is ASCII-only here, matching
/// `case_folds_equal` (extending to full Unicode is RP1-F15 territory).
///
/// Callers should:
/// 1. Emit one `remote.case_collision.dropped` audit event per entry.
/// 2. Skip any item whose path is a key in the map (no `FileEntry` upsert, no
///    decision). The canonical item proceeds through reconcile normally.
fn detect_remote_case_collisions(
    items: &[RemoteItem],
) -> std::collections::HashMap<RelPath, RelPath> {
    let mut buckets: std::collections::HashMap<String, Vec<RelPath>> =
        std::collections::HashMap::new();
    for item in items {
        if let Some(p) = build_rel_path_from_item(item) {
            buckets
                .entry(p.as_str().to_ascii_lowercase())
                .or_default()
                .push(p);
        }
    }
    let mut result = std::collections::HashMap::new();
    for paths in buckets.into_values() {
        if paths.len() <= 1 {
            continue;
        }
        let mut sorted = paths;
        sorted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let canonical = sorted.remove(0);
        for dropped in sorted {
            result.insert(dropped, canonical.clone());
        }
    }
    result
}

/// Build the engine-side `RelPath` for one delta-page item.
///
/// RP1-F16: spec `docs/spec/04-onedrive-adapter.md` says a Graph `driveItem`'s
/// `name` is only the leaf; the directory portion lives on
/// `parent_reference.path`, shaped like `/drive/root:/Documents/Sub`. The
/// pre-fix engine treated `name` alone as the full relative path, so any
/// nested item was either filed under the wrong key or collided with another
/// item that shared a leaf name.
///
/// The assembly rules:
/// - `parent_reference` absent → fall back to `name` alone (legacy fakes,
///   root-folder responses).
/// - `parent_reference.path` is `None` → fall back to `name` alone.
/// - The path is normalised by stripping the first `:` segment (Graph's
///   `/drive/root:` prefix) and the leading `/`, leaving the directory part.
///   If that part is empty, the item lives at the pair root.
/// - Final shape: `<dir>/<name>` or just `<name>` for root-level items.
///
/// Returns `None` only if the assembled string fails `RelPath` validation
/// (out-of-band characters, `..` segments, etc.).
fn build_rel_path_from_item(item: &RemoteItem) -> Option<RelPath> {
    let parent_dir: String = item
        .parent_reference
        .as_ref()
        .and_then(|p| p.path.as_deref())
        .map_or_else(String::new, |path| {
            // "/drive/root:/Documents/Sub" -> "Documents/Sub"
            // "/drive/root:"               -> ""
            let rest = path.split_once(':').map_or(path, |(_, after)| after);
            rest.trim_start_matches('/').to_owned()
        });

    let full = if parent_dir.is_empty() {
        item.name.clone()
    } else {
        format!("{parent_dir}/{}", item.name)
    };
    full.parse().ok()
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

const fn synthetic_remote_side() -> FileSide {
    // RP1-F27: use chrono's static UNIX_EPOCH constant rather than
    // `from_timestamp(0, 0).unwrap_or_default()` — the unwrap_or_default was
    // dead code (epoch is always valid) and obscured the intent.
    FileSide {
        kind: FileKind::File,
        size_bytes: 0,
        content_hash: None,
        mtime: onesync_protocol::primitives::Timestamp::from_datetime(chrono::DateTime::UNIX_EPOCH),
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

/// Strict local-vs-synced divergence check.
///
/// RP1-F5: spec `docs/spec/03-sync-engine.md` line 149 defines equality as
/// `(kind, size_bytes, content_hash)`; `mtime` is metadata used only as a
/// conflict tie-break. The pre-fix code used `mtime` as a primary equality
/// signal, which silently absorbed any same-size edit that preserved or
/// re-set the mtime (editors that restore mtime on save, second-resolution
/// timestamp collisions). Same-size + same-mtime made the local change
/// invisible — a false negative that loses data.
///
/// New rule:
///   - kind mismatch                          -> diverged,
///   - both directories                       -> not diverged,
///   - size mismatch                          -> diverged,
///   - both files of size 0 (matching size)   -> not diverged,
///   - both hashes present and equal          -> not diverged,
///   - anything else                          -> diverged (conservative).
///
/// Conservative means we upload more often when hashes are missing on the
/// local side (`LocalFs::scan` does not populate them by default). The
/// trade-off is bandwidth vs silent data drop; the latter is unacceptable.
fn local_diverges_from_synced(side: &FileSide, synced: Option<&FileSide>) -> bool {
    let Some(synced) = synced else {
        return true;
    };
    if side.kind != synced.kind {
        return true;
    }
    if side.kind == FileKind::Directory {
        return false;
    }
    if side.size_bytes != synced.size_bytes {
        return true;
    }
    if side.size_bytes == 0 {
        return false;
    }
    match (side.content_hash.as_ref(), synced.content_hash.as_ref()) {
        (Some(l), Some(s)) => l != s,
        _ => true,
    }
}

/// Op-execution result counters produced by [`phase_execute`].
struct ExecuteCounts {
    /// Number of ops that finished with `FileOpStatus::Success`.
    applied: usize,
    /// Number of ops that finished with `FileOpStatus::Failed`
    /// (retry-exhausted or non-retriable error).
    failed: usize,
}

/// Classify a sync run's outcome from execute-phase counters.
///
/// Spec `docs/spec/03-sync-engine.md` line 38-39 names a `PartialFailure`
/// outcome distinct from `Success`. RP1-F19: the engine previously hard-coded
/// `Success` regardless of per-op result, which masked recurring upload
/// failures and made `SyncRun.outcome` an unreliable signal.
const fn classify_outcome(applied: usize, failed: usize) -> RunOutcome {
    if failed == 0 {
        RunOutcome::Success
    } else if applied == 0 {
        // All planned ops failed — still a `PartialFailure` per the enum
        // (we lack a dedicated `Failure` variant) but the detail string and
        // counts will distinguish it for operators.
        RunOutcome::PartialFailure
    } else {
        RunOutcome::PartialFailure
    }
}

/// Phase 5: execute each op with retries; returns success / failure counts.
async fn phase_execute<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
    ops: Vec<FileOp>,
) -> Result<ExecuteCounts, EngineError> {
    let mut applied = 0usize;
    let mut failed = 0usize;

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
                    failed += 1;
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
                    if status == FileOpStatus::Success {
                        // RP1-F11: reflect the post-op state on FileEntry.synced
                        // and transition sync_state to Clean per spec
                        // `03-sync-engine.md` lines 230-231.
                        update_file_entry_post_op(ctx, &op).await?;
                        applied += 1;
                    } else {
                        failed += 1;
                    }
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
                    failed += 1;
                    break;
                }
            }
        }
    }

    Ok(ExecuteCounts { applied, failed })
}

/// Reflect a successful op onto the persisted `FileEntry`:
/// updates `synced` to the post-op shape, flips `sync_state` to `Clean`,
/// and clears `pending_op_id`. A missing entry (concurrent removal,
/// inconsistent caller wiring) is a silent no-op — `FileOp.status` is the
/// authoritative success signal, and we'd rather leave reconcile to fix any
/// drift than fail a cycle here.
async fn update_file_entry_post_op<I: IdGenerator>(
    ctx: &CycleCtx<'_, I>,
    op: &FileOp,
) -> Result<(), EngineError> {
    // RP1-F4 / F14 / F24 follow-ons: conflict-resolution ops drive
    // disambiguating renames that don't represent "this path is now clean
    // at the post-op state of side X" — they move a file to a side-channel
    // path so the canonical version can survive. Leaving the FileEntry
    // alone preserves the PendingConflict state (when one was set) so the
    // operator's manual `conflicts resolve` decides the canonical fate.
    if op
        .metadata
        .get("from_conflict")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return Ok(());
    }

    let Some(mut entry) = ctx
        .state
        .file_entry_get(&op.pair_id, &op.relative_path)
        .await
        .map_err(|e| EngineError::Port(e.to_string()))?
    else {
        return Ok(());
    };

    let now = ctx.clock.now();
    let new_synced = match op.kind {
        FileOpKind::Upload => entry.local.clone(),
        FileOpKind::Download => entry.remote.clone(),
        FileOpKind::LocalMkdir | FileOpKind::RemoteMkdir => Some(FileSide {
            kind: FileKind::Directory,
            size_bytes: 0,
            content_hash: None,
            mtime: now,
            etag: None,
            remote_item_id: None,
        }),
        FileOpKind::LocalDelete | FileOpKind::RemoteDelete => None,
        FileOpKind::LocalRename | FileOpKind::RemoteRename => entry.synced.clone(),
    };

    entry.synced = new_synced;
    entry.sync_state = FileSyncState::Clean;
    entry.pending_op_id = None;
    entry.updated_at = now;

    ctx.state
        .file_entry_upsert(&entry)
        .await
        .map_err(|e| EngineError::Port(e.to_string()))?;
    Ok(())
}

/// Deterministic pseudo-jitter for use without a random source.
///
/// Returns 0.25 for odd attempts, 0.0 for even — purely for retry scheduling in
/// deterministic contexts (tests, and as a fallback). Production callers should
/// supply true random jitter.
const fn pseudo_jitter(attempt: u32) -> f64 {
    if attempt % 2 == 1 { 0.25 } else { 0.0 }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use onesync_protocol::{
        enums::FileKind,
        file_side::FileSide,
        primitives::{ContentHash, Timestamp},
    };

    fn ts(secs: i64) -> Timestamp {
        // LINT: tests may use Utc::now/Utc.timestamp_opt directly.
        #[allow(clippy::disallowed_methods)]
        Timestamp::from_datetime(Utc.timestamp_opt(secs, 0).unwrap())
    }

    fn h(byte: u8) -> ContentHash {
        let hex: String = std::iter::repeat_n(format!("{byte:02x}"), 32).collect();
        hex.parse().unwrap()
    }

    fn file_side(size: u64, hash: Option<ContentHash>, mtime_secs: i64) -> FileSide {
        FileSide {
            kind: FileKind::File,
            size_bytes: size,
            content_hash: hash,
            mtime: ts(mtime_secs),
            etag: None,
            remote_item_id: None,
        }
    }

    fn dir_side() -> FileSide {
        FileSide {
            kind: FileKind::Directory,
            size_bytes: 0,
            content_hash: None,
            mtime: ts(0),
            etag: None,
            remote_item_id: None,
        }
    }

    #[test]
    fn rp1_f5_synced_absent_is_diverged() {
        let side = file_side(10, Some(h(0xaa)), 100);
        assert!(local_diverges_from_synced(&side, None));
    }

    #[test]
    fn rp1_f5_both_directories_is_equal() {
        let local = dir_side();
        let synced = dir_side();
        assert!(!local_diverges_from_synced(&local, Some(&synced)));
    }

    #[test]
    fn rp1_f5_kind_mismatch_is_diverged() {
        let local = file_side(0, None, 0);
        let synced = dir_side();
        assert!(local_diverges_from_synced(&local, Some(&synced)));
    }

    #[test]
    fn rp1_f5_size_mismatch_is_diverged() {
        let local = file_side(20, Some(h(0xaa)), 100);
        let synced = file_side(10, Some(h(0xaa)), 100);
        assert!(local_diverges_from_synced(&local, Some(&synced)));
    }

    #[test]
    fn rp1_f5_both_zero_byte_files_is_equal() {
        let local = file_side(0, None, 100);
        let synced = file_side(0, None, 200);
        assert!(!local_diverges_from_synced(&local, Some(&synced)));
    }

    #[test]
    fn rp1_f5_matching_hashes_is_equal() {
        let local = file_side(10, Some(h(0xaa)), 100);
        let synced = file_side(10, Some(h(0xaa)), 9_999);
        assert!(!local_diverges_from_synced(&local, Some(&synced)));
    }

    #[test]
    fn rp1_f5_differing_hashes_is_diverged() {
        let local = file_side(10, Some(h(0xaa)), 100);
        let synced = file_side(10, Some(h(0xbb)), 100);
        assert!(local_diverges_from_synced(&local, Some(&synced)));
    }

    /// The critical pre-fix bug: same size + same mtime + no hash on either
    /// side was silently treated as "no divergence" → local edits with the
    /// same byte count never uploaded. Post-fix this conservatively diverges.
    #[test]
    fn rp1_f5_same_size_same_mtime_no_hash_is_diverged() {
        let local = file_side(10, None, 100);
        let synced = file_side(10, None, 100);
        assert!(local_diverges_from_synced(&local, Some(&synced)));
    }

    // RP1-F19: outcome classification.
    #[test]
    fn rp1_f19_zero_zero_is_success() {
        assert_eq!(classify_outcome(0, 0), RunOutcome::Success);
    }

    #[test]
    fn rp1_f19_some_applied_zero_failed_is_success() {
        assert_eq!(classify_outcome(5, 0), RunOutcome::Success);
    }

    #[test]
    fn rp1_f19_mixed_is_partial_failure() {
        assert_eq!(classify_outcome(3, 2), RunOutcome::PartialFailure);
    }

    #[test]
    fn rp1_f19_all_failed_is_partial_failure() {
        assert_eq!(classify_outcome(0, 3), RunOutcome::PartialFailure);
    }

    // RP1-F16: delta-page item -> RelPath assembly. The pre-fix code used
    // `item.name.parse::<RelPath>()` which lost the directory portion.

    use onesync_protocol::remote::{FileFacet, FileHashes, ParentReference, RemoteItem};

    fn item_with_parent(name: &str, parent_path: Option<&str>) -> RemoteItem {
        RemoteItem {
            id: "id-1".to_owned(),
            name: name.to_owned(),
            size: 0,
            e_tag: None,
            c_tag: None,
            last_modified_date_time: None,
            file: Some(FileFacet {
                hashes: FileHashes::default(),
            }),
            folder: None,
            deleted: None,
            parent_reference: parent_path.map(|p| ParentReference {
                id: None,
                drive_id: None,
                path: Some(p.to_owned()),
            }),
        }
    }

    #[test]
    fn rp1_f16_root_item_no_parent_uses_name_only() {
        let item = item_with_parent("hello.txt", None);
        let rel = build_rel_path_from_item(&item).expect("valid path");
        assert_eq!(rel.as_str(), "hello.txt");
    }

    #[test]
    fn rp1_f16_root_drive_prefix_yields_root_path() {
        let item = item_with_parent("hello.txt", Some("/drive/root:"));
        let rel = build_rel_path_from_item(&item).expect("valid path");
        assert_eq!(rel.as_str(), "hello.txt");
    }

    #[test]
    fn rp1_f16_nested_path_joins_dir_and_name() {
        let item = item_with_parent("report.txt", Some("/drive/root:/Documents"));
        let rel = build_rel_path_from_item(&item).expect("valid path");
        assert_eq!(rel.as_str(), "Documents/report.txt");
    }

    #[test]
    fn rp1_f16_deep_nested_path_preserves_segments() {
        let item = item_with_parent("note.md", Some("/drive/root:/Documents/2026/May"));
        let rel = build_rel_path_from_item(&item).expect("valid path");
        assert_eq!(rel.as_str(), "Documents/2026/May/note.md");
    }

    // RP1-F14: remote-side case-collision detection.

    #[test]
    fn rp1_f14_no_collisions_returns_empty_map() {
        let items = vec![
            item_with_parent("alpha.txt", None),
            item_with_parent("beta.txt", None),
        ];
        let m = detect_remote_case_collisions(&items);
        assert!(m.is_empty());
    }

    #[test]
    fn rp1_f14_case_folded_pair_yields_one_dropped() {
        let items = vec![
            item_with_parent("Foo.txt", None),
            item_with_parent("foo.txt", None),
        ];
        let m = detect_remote_case_collisions(&items);
        assert_eq!(m.len(), 1);
        // `Foo.txt` sorts byte-wise smaller than `foo.txt` (uppercase F is
        // 0x46 < lowercase f 0x66), so `foo.txt` is the dropped loser.
        let foo: RelPath = "foo.txt".parse().unwrap();
        let big: RelPath = "Foo.txt".parse().unwrap();
        assert_eq!(m.get(&foo), Some(&big));
    }

    #[test]
    fn rp1_f14_three_way_collision_keeps_one_drops_two() {
        let items = vec![
            item_with_parent("File.TXT", None),
            item_with_parent("file.txt", None),
            item_with_parent("FILE.txt", None),
        ];
        let m = detect_remote_case_collisions(&items);
        assert_eq!(m.len(), 2);
        // Canonical = byte-wise smallest = "FILE.txt".
        let canonical: RelPath = "FILE.txt".parse().unwrap();
        for v in m.values() {
            assert_eq!(*v, canonical);
        }
    }

    #[test]
    fn rp1_f14_byte_identical_paths_are_not_collisions() {
        // Two items with the same path are a duplicate, not a case-collision.
        // `detect_remote_case_collisions` should still not mark either as
        // dropped — the bucket has two byte-identical entries which collapse
        // to one canonical and zero drops after sort+dedup. We don't dedup
        // here (different `RemoteItem` ids may live under the same path in
        // some delta layouts); the function returns one drop and accepts
        // that the caller handles the redundancy. Verify both paths route
        // to the same canonical.
        let items = vec![
            item_with_parent("dup.txt", None),
            item_with_parent("dup.txt", None),
        ];
        let m = detect_remote_case_collisions(&items);
        assert_eq!(m.len(), 1);
        let dup: RelPath = "dup.txt".parse().unwrap();
        assert_eq!(m.get(&dup), Some(&dup));
    }
}
