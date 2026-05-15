# Review: Sync engine (onesync-core) — 2026-05-15

## Scope
- crates/onesync-core/src/lib.rs
- crates/onesync-core/src/limits.rs
- crates/onesync-core/src/engine/{mod,types,planner,reconcile,executor,scheduler,retry,cycle,conflict,case_collision,observability}.rs
- crates/onesync-core/src/ports/{mod,remote_drive,audit_sink,clock,id_generator,local_fs,state,token_vault}.rs
- Integration tests: crates/onesync-core/tests/engine_cycle_{clean,local_upload,remote_dirty,conflict,case_collision}.rs (contract-only)

## Method
- Read 03-sync-engine.md + 01-domain-model.md as the authoritative spec, then traced sources from outermost in (cycle → planner → reconcile → executor → scheduler → retry → conflict/case_collision/observability) plus ports.
- Applied **Fault Localization** template for reconcile branch coverage and conflict-policy determinism.
- Applied **Patch Verification / Execution Trace** template for executor ordering, case-collision sequencing, scheduler debouncing, and cycle-boundary state.
- Forced **FUNCTION RESOLUTION** for every cross-module call I assert about (planner→reconcile, cycle→executor, executor→ports).
- Streamed findings to this file as soon as identified (no in-head buffering).

## Findings

### F1. Reconcile both-sides-converged path is missing — falsely flags as conflict — BUG
**Location:** `crates/onesync-core/src/engine/reconcile.rs:106-114`
**Severity:** BUG
**Summary:** When `(local_changed, remote_changed) == (true, true)` the code immediately produces `DecisionKind::Conflict`. The spec (03-sync-engine.md table at lines 140-147) explicitly distinguishes a fifth row: *both sides diverged from `synced` but `local == remote`* → mark `Clean` with `synced = local`. That branch is never taken.
**Evidence:**
```rust
match (local_changed, remote_changed) {
    ...
    (true, true) => {
        DecisionKind::Conflict { winner: ConflictSide::Remote, loser_path: entry.relative_path.clone() }
    }
}
```
**Reasoning (Fault Localization):**
- PREMISE: spec defines five distinct outcomes from joining `synced × local × remote`. The first four are handled; the fifth (`local == remote ≠ synced`) is not.
- DIVERGENCE: `reconcile_both` never compares `local` to `remote` directly — only each against `synced`. A common case is "user edited the same file in two places to identical content" or "delta cursor was lost so both sides look new but match"; both should be no-op or `Clean` updates, not conflicts.
- VERIFICATION: would adding `if local.identifies_same_content_as_remote(remote) → NoOp/Clean` fix the symptom? Yes — it is exactly the missing branch.
**Suggested direction:** Before the `(true, true) → Conflict` branch, add a content-equality check between `local` and `remote`; if equal, treat as `NoOp` (or surface a "promote both to synced" decision). Update tests to lock this in (`initial_sync_collision_is_conflict` actually exercises an opposite case — same size, hashes both `None`, so equality is undefined).

### F2. Remote-vs-synced change detection silently falls back to size-only comparison — BUG
**Location:** `crates/onesync-core/src/engine/reconcile.rs:118-130`
**Severity:** BUG
**Summary:** `remote_differs_from_synced` only compares ETag when **both** `synced.etag` and `remote.e_tag` are present; otherwise it falls back to `size_bytes != remote.size`. A same-size edit (very common: trivial text edits, in-place log appends that rewrite the file) is invisible. The spec says equality is `(kind, size, hash)` (03-sync-engine.md line 149, 332); hash is never consulted here.
**Evidence:**
```rust
if let Some(etag) = synced.etag.as_ref()
    && let Some(remote_etag) = remote.e_tag.as_deref()
{
    return etag.as_str() != remote_etag;
}
synced.size_bytes != remote.size  // hash never checked
```
**Reasoning (Patch Verification / Function Resolution):**
- FUNCTION RESOLUTION: `local_differs_from_synced` calls `FileSide::identifies_same_content_as` (presumably checks hash). The remote side uses a different, weaker rule.
- EXECUTION TRACE: synced has `size=100, hash=H1`. Remote returns `size=100, e_tag=None` (some delta pages omit it for folders / certain types). Local edit changes content to size 100 with `hash=H2`. Result: `remote_changed=false, local_changed=true` → `Upload` — and the upload runs even though remote ALSO changed silently. Worse: if remote also changed but ETag is missing, the change is silently dropped.
- EDGE CASE: this asymmetry breaks the canonical equality definition. Spec explicitly forbids mtime; size alone is even weaker.
**Suggested direction:** Compare on the canonical tuple from `FileSide::identifies_same_content_as`. If `remote.e_tag` is missing, fall back to `synced.content_hash` vs `remote.file.hashes.*`. Never silently size-compare without flagging the fallback.

### F3. Conflict decision hard-codes `winner = Remote` and `loser_path = relative_path` — BUG
**Location:** `crates/onesync-core/src/engine/reconcile.rs:109-112`
**Severity:** BUG
**Summary:** Comment says "The caller resolves the conflict with `pick_winner_and_loser`" but the produced `Decision` already names `winner: Remote` and `loser_path: relative_path` (the canonical path, not the suffixed conflict copy). Anything downstream that trusts these fields (e.g. planner direct-passthrough — see planner findings) gets wrong values; only the conflict-policy module gives correct ones.
**Evidence:** as above.
**Reasoning:** The data carrier is the same `DecisionKind::Conflict` shape used after policy resolution; setting bogus placeholder values is a sharp edge — the type does not distinguish "pre-policy stub" from "post-policy decision". Several call sites can mistake one for the other.
**Suggested direction:** Either (a) introduce a `DecisionKind::ConflictDetected` placeholder distinct from `Conflict { winner, loser_path }` so the type guarantees later resolution, or (b) wrap the winner/loser fields in `Option`.

### F4. Planner drops every `Conflict` decision — conflicts never produce ops — BUG
**Location:** `crates/onesync-core/src/engine/planner.rs:38-42` together with `crates/onesync-core/src/engine/cycle.rs:96-110`
**Severity:** BUG
**Summary:** `planner::plan` calls `decision.kind.to_file_op_kind()`, which returns `None` for `DecisionKind::Conflict { .. }`, then `continue`s the loop. So when `reconcile_one` emits a `Conflict`, the planner silently drops it. Looking at `cycle::run_cycle`, no other code path materialises the conflict-policy op sequence (rename loser → propagate rename → propagate winner content → insert Conflict row) described in 03-sync-engine.md lines 194-208. The only path that records a `Conflict` row is the **case-collision** handler in `cycle::handle_case_collision`; ordinary content conflicts never produce a rename or a row.
**Evidence:**
- `planner.rs:38-42`: `let Some(kind) = decision.kind.to_file_op_kind() else { continue; };`
- `types.rs:75`: `Self::Conflict { .. } | Self::NoOp => None`
- `cycle.rs:96-110`: only `phase_delta_reconcile` + `phase_local_uploads` populate decisions; nothing consumes a `Conflict` decision before `plan()`.
- `conflict::pick_winner_and_loser` exists but is **not called from `cycle.rs` or `planner.rs`** (grep `pick_winner_and_loser` to confirm — see Method).
**Reasoning (Patch Verification — Function Resolution):**
- FUNCTION RESOLUTION: `cycle::run_cycle` → `phase_delta_reconcile` → `reconcile_one` (emits `Conflict`) → `plan` → `continue`. The decision is discarded, `conflicts_detected` counter is incremented at `cycle.rs:201-203` (for telemetry only). No call to `pick_winner_and_loser`.
- EXECUTION TRACE: user edits `notes.txt` locally and remotely. Delta page returns updated `notes.txt`. `reconcile_one` produces `Conflict`. `conflicts_detected` reaches 1. `plan` outputs zero ops. Cycle finishes "successful", local edit is never uploaded, remote edit is never downloaded, no `Conflict` row exists, user has no UI signal that the divergence existed. Next cycle: same outcome (state hasn't changed). **Silent data divergence persists indefinitely.**
**Suggested direction:** Either materialise conflicts as ops in the planner (consuming `pick_winner_and_loser`), or have `cycle.rs` add a dedicated `phase_conflicts` between reconcile and plan that emits the rename + transfer sequence and inserts the `Conflict` row, mirroring the case-collision handler.

### F5. Local divergence check uses size+mtime, ignoring content — BUG
**Location:** `crates/onesync-core/src/engine/cycle.rs:460-478` (`local_diverges_from_synced`)
**Severity:** BUG
**Summary:** During `phase_local_uploads`, the engine decides whether to upload by comparing only `size_bytes` and `mtime` against `synced`. The doc-comment acknowledges "we deliberately avoid hashing here … false positives are acceptable; false negatives (skipping a real edit) would be a correctness bug" — but the implementation **does** produce false negatives: a same-size edit that preserves mtime (`touch -r`, many editors that restore mtime after save, file-tagging tools) returns "no divergence" and the upload is skipped.
**Evidence:**
```rust
if side.size_bytes != synced.size_bytes { return true; }
side.mtime != synced.mtime  // both same → returns false even if content differs
```
Spec line 149: equality is `(kind, size, content_hash)`; mtime is explicitly **not** part of equality and is only used for tie-breaks.
**Reasoning (Patch Verification — Function Resolution):**
- FUNCTION RESOLUTION: `phase_local_uploads` → `local_diverges_from_synced(side, synced)`. `side` was filled by `LocalFs::scan`, whose `FileSide` has `content_hash: None` for performance. So the engine has the choice between (a) calling `LocalFs::hash` lazily on the candidate, or (b) using the size-mtime heuristic. It picked (b).
- EXECUTION TRACE: editor opens `notes.txt` (size 200, mtime T1, hash H1), user replaces all text with a new same-size paragraph and the editor preserves mtime (rare but possible) or two edits collapse within `Timestamp` second-precision (much more common): size unchanged, mtime equal at second resolution. `local_diverges_from_synced` returns `false`. Local-side change is **not uploaded**. Remote keeps stale content forever.
- DIVERGENCE: even a 1-second mtime resolution is plausible. macOS supports nanosecond mtime but if anything along the path truncates (some scan implementations stat with `mtime` only), false-negative is easy.
**Suggested direction:** When size matches but the decision is load-bearing, call `LocalFs::hash` on the suspect path and compare against `synced.content_hash`. The doc-comment claims this is "expensive on every cycle" — but the check only runs when an entry has a `synced` snapshot AND wasn't already covered by remote delta, so the working set is small. Alternatively, gate on FSEvents having seen the path during the cycle window; if FSEvents reports a Modified event, force the upload regardless of metadata.

### F6. Remote-side change-detection on `synced.etag = None` falls back to size — BUG (duplicate of F2 root cause but distinct manifestation)
**Location:** `crates/onesync-core/src/engine/reconcile.rs:118-130`
**Severity:** BUG
**Summary:** When `synced.etag = None` (which is exactly the state during the very first cycle and any cycle after a manual etag-clear), the function compares `synced.size_bytes` against `remote.size`. A same-size content change passes through as "no remote change". See F2 — flagged separately to make the call-graph reach clear.

### F7. Executor implements only 4 of 8 op kinds; rest return `NotImplemented` and surface as fatal — CONCERN/BUG
**Location:** `crates/onesync-core/src/engine/executor.rs:50-63` and `:34-43` (`is_retriable`)
**Severity:** BUG
**Summary:** `execute()` handles `LocalMkdir`, `LocalDelete`, `Download`, `Upload`. The fall-through pattern `kind => Err(ExecError::NotImplemented { kind })` covers `RemoteMkdir`, `RemoteDelete`, `LocalRename`, `RemoteRename`. `is_retriable(NotImplemented) = false`, so `cycle::phase_execute` marks these ops `Failed` immediately at first attempt. Concretely:
- Any **remote-side new folder** (planner emits `RemoteMkdir`) fails. Spec phase-3b reconcile-table row "differs/equal" should enqueue `RemoteMkdir` for directories.
- Any **remote deletion of a local file** is fine (LocalDelete works), but a local deletion will fail (RemoteDelete is unimplemented).
- All rename decisions fail (LocalRename, RemoteRename) — directly relevant for the conflict policy (rename-loser step) IF the conflict policy were ever invoked (see F4).
**Evidence:** `executor.rs:61` — `kind => Err(ExecError::NotImplemented { kind })`. Combined with `cycle.rs:519-533` which treats non-retriable errors by marking the op `Failed` and continuing.
**Reasoning (Execution Trace):** User creates `docs/new/` locally → `phase_local_uploads` emits `RemoteMkdir` (`cycle.rs:295-299`) → planner orders it as mkdir-first → `execute` returns `NotImplemented` → op marked Failed → audit event `file_op_failed` → directory never created on remote → all subsequent uploads inside `docs/new/` fail with parent-not-found (or are routed under root, depending on adapter). **No retry, no surfacing to user beyond audit log.**
**Suggested direction:** Either remove the un-implemented branches from the planner output until they have executor coverage, or implement them. The current shape promises a feature the engine cannot deliver and turns a planning bug (missing op) into a silent runtime failure.

### F8. Retry counter is per-op-loop, not persisted; resets across cycles — CONCERN/BUG
**Location:** `crates/onesync-core/src/engine/cycle.rs:487-535`
**Severity:** CONCERN (tilting BUG by spec)
**Summary:** `phase_execute` loops over ops and initialises `let mut attempt: u32 = 0;` for each op (line 493). The op already carries `attempts: u32` from `FileOp` (set to 0 in the planner) and `op.attempts = attempt + 1` is written into the local copy (line 506), but **only the local copy** — `state.op_update_status` doesn't take `attempts` as an argument. So on the next cycle the persisted `attempts` is still 0, and the per-op budget restarts.
Cross-check against spec line 232: "transitions to `Backoff`, increments `attempts`, schedules re-execution after RETRY_BACKOFF_BASE_MS * 2^attempts" — this presupposes durable `attempts`. The implementation throws it away.
**Evidence:**
```rust
let mut attempt: u32 = 0;  // local only
loop {
    match retry_decision(attempt, pseudo_jitter(attempt)) { ... }
    op.attempts = attempt + 1;  // mutates local FileOp
    match execute(&op, ...).await { ... attempt += 1 ... }
}
```
`op_update_status` (state.rs:65-69) takes only `id` and `status` — no `attempts` parameter.
**Reasoning:** A transient retryable error within one cycle exhausts the budget (5 attempts) inside the for-loop; if the op was instead deferred and re-tried on a subsequent cycle, the persisted `attempts=0` would give it 5 fresh tries each cycle — effectively unbounded retries.
**Suggested direction:** Decide the policy (per spec the count is durable across cycles) and either (a) extend `StateStore::op_update_status` to take `attempts`, or (b) add a dedicated `op_update_attempts` method. The current design also conflates "in-cycle retries with backoff sleep" with "cross-cycle reschedule" — there is no actual sleep between attempts.

### F9. Retry loop in `phase_execute` ignores the computed backoff delay — CONCERN
**Location:** `crates/onesync-core/src/engine/cycle.rs:494-535`
**Severity:** CONCERN
**Summary:** `retry_decision` returns `Backoff { delay_ms }` but the loop never sleeps; it busy-retries. There is no `tokio::time::sleep`, no scheduling onto a later cycle, nothing. Effective behaviour: up to `RETRY_MAX_ATTEMPTS` adapter calls fired back-to-back. For HTTP 429 (`Throttled { retry_after_s }`) this directly violates the server-supplied retry-after.
**Evidence:** as above; no `tokio::time::sleep` import in `cycle.rs`.
**Reasoning:** The spec is explicit (line 232 + Throttling section line 260-265): `Throttled { retry_after_s }` must move ops to `Backoff` for *at least* that long. The current code maps it through `is_retriable` → immediate retry → almost certainly another 429. Microsoft Graph will start banning the account after enough of these.
**Suggested direction:** Either honour the delay (`tokio::time::sleep(Duration::from_millis(delay_ms))`) inside the loop — acceptable for short delays — or move retried ops to a "deferred" queue that the scheduler picks up after the deadline. For `Throttled`, the `retry_after_s` is load-bearing and must be respected.

### F10. Cycle never persists the new delta cursor — CONCERN/BUG
**Location:** `crates/onesync-core/src/engine/cycle.rs:77-151`
**Severity:** BUG
**Summary:** `phase_delta_reconcile` extracts `delta_token` from the page and threads it into `CycleSummary.delta_token`, but `run_cycle` never calls `state.pair_upsert` or any state method to persist the new cursor against the `Pair`. The summary field has a doc-comment "The scheduler persists this on the `Pair`" but no scheduler code yet exists (scheduler.rs body is a stub). Spec line 102-103: "The cursor is advanced **only after** the engine has persisted every item in the page into `FileEntry.remote`."
**Evidence:** `cycle.rs:134-140` — `CycleSummary { ... delta_token, }`. No call to `pair_upsert` or `pair_set_cursor` anywhere in the cycle. `Pair` is never even loaded.
**Reasoning:** The cycle is half-stateful: it inserts file entries and ops, but the cursor advance lives only in an in-memory summary returned to a caller that does not yet exist. Two failure modes flow from this:
1. Re-running a cycle re-fetches the full delta page (since cursor never advances), which on Graph is at best wasteful and at worst causes duplicate event processing.
2. If the caller forgets to persist the summary, the engine never makes forward progress on remote changes.
Also note spec line 103-105 requires page items to be persisted into `FileEntry.remote` before cursor advance — but the cycle code doesn't update `FileEntry.remote` at all in `phase_delta_reconcile`. It only emits decisions; the `synced` field is supposed to be updated post-execute (per phase ordering, line 230-231), and there's no code in `cycle.rs` that does this either.
**Suggested direction:** Either bring cursor advance and `FileEntry.remote`/`FileEntry.synced` updates into the engine (preferred per spec), or document that the engine is currently a "decision computer" and the persistence layer lives elsewhere — and audit every caller to make sure they wire it up.

### F11. `FileEntry.synced` is never updated after a successful op — BUG
**Location:** `crates/onesync-core/src/engine/cycle.rs:487-539`, executor (no `synced` update anywhere)
**Severity:** BUG
**Summary:** Spec phase-3 of op execution (line 230-231): "On success: updates `FileEntry.synced` to match the post-op state and transitions to `Success`." The implementation only calls `state.op_update_status(...)`; the `FileEntry` is left with its old `synced` snapshot. Next cycle, the same path will still appear divergent and an upload/download will be re-emitted.
**Evidence:** `phase_execute` only ever calls `state.op_update_status` and `state.op_insert`. No `state.file_entry_upsert` post-success.
**Reasoning:** Combined with F5+F2: a successful upload leaves `synced.size = old_size, synced.etag = None`. The next reconcile will compare the (unchanged) local against the (unchanged) `synced` and treat the local as still dirty. Or, if the executor wrote the file but a remote change also occurred, reconcile will see `local_changed + remote_changed = (true, true)` → conflict (per F1 which mis-classifies). This is the cascade that turns one missed update into recurring sync churn.
**Suggested direction:** Make the executor return the post-op `FileSide` (already returned by `LocalFs::write_atomic`) and have `phase_execute` update `FileEntry.synced` accordingly. Mirror this for `upload_small` / `download` which need the remote-side post-state.

### F12. `phase_execute` halts the entire cycle on any `StateStore::op_insert` error — CONCERN
**Location:** `crates/onesync-core/src/engine/cycle.rs:488-491`
**Severity:** CONCERN
**Summary:** A transient state-store failure for one op (e.g. unique-constraint race against another writer) propagates `EngineError::Port` and abandons every subsequent op in the queue. Per spec, transient adapter errors should retry or skip the offending op, not torpedo the cycle. Same pattern at lines 498-500, 511-512, 521-523.
**Suggested direction:** Wrap state-store failures per-op so a poison op doesn't block the rest of the plan; surface the failure as `op_failed` and continue.

### F13. Conflict-loser rename name diverges from spec — NIT/BUG
**Location:** `crates/onesync-core/src/engine/conflict.rs:65-87`
**Severity:** NIT (BUG for spec conformance)
**Summary:** Spec line 178 mandates `<stem> (conflict <YYYY-MM-DDTHH-MM-SSZ> from <host>).<ext>` — i.e. the UTC timestamp of conflict detection is part of the filename. The implementation produces `<stem> (conflict copy from <host>).<ext>`, no timestamp, with a different literal ("conflict copy" vs "conflict <date>"). This means two conflicts on the same path will collide and require the disambiguation retry path (the `.1`, `.2`, …).
**Suggested direction:** Either update the spec (acceptable; "conflict copy from <host>" is reasonable) or add the timestamp. Whichever way, sync the two.

### F14. Case-collision detection skips a remote-only collision pair — BUG
**Location:** `crates/onesync-core/src/engine/cycle.rs:233-312` (`phase_local_uploads`) and `:316-323` (`find_case_collision`)
**Severity:** BUG
**Summary:** The case-collision policy is only run *from the local-scan side*. Trace the asymmetric cases from the brief:
1. **Remote has both `Foo.txt` and `foo.txt`, local sees only one (APFS folds them).** `phase_delta_reconcile` processes each remote item independently. For the local-folded name, both `Foo.txt` and `foo.txt` reconcile against the **same** `FileEntry` (whatever path was stored first), and whichever delta order arrives first wins; the second will produce a `Conflict` decision (per the broken reconcile, see F1) or a download decision. The download then overwrites — silent loss of one remote file. No case-collision row is recorded because `find_case_collision` is never called from this path.
2. **Local has `Foo.txt`, remote renames it to `foo.txt`.** Delta returns `foo.txt`. Reconcile compares `FileEntry@Foo.txt` keyed by exact path; `state.file_entry_get(&pair, &rel_path)` is called with `foo.txt`, returns `None`, so it's treated as a new remote download. Local `Foo.txt` is unchanged (no delete decision). APFS treats download to `foo.txt` as overwriting `Foo.txt` (same inode, lowercase wins or case is preserved depending on the path written). User-observable: `Foo.txt` now has remote content, the original local content is lost. No conflict surfaced.
**Reasoning (Execution Trace):** Cycle has no key-folding step on remote delta. `HashMap<RelPath, RemoteItem>` keys remote items by exact path, while `FileEntry` lookup is also exact-path. APFS may already merge two local "files" the engine thinks are distinct. The local-side check (`find_case_collision`) only fires when the local scan emits a path not in `already_decided` — which is exactly when the local file collided with a remote-only path.
**Suggested direction:** Add a fold-based pass on the remote delta: bucket remote items by lowercased path, and if any bucket has >1 entry, emit a `Conflict` on the remote side (auto-resolve by picking one; record the loser). On the local side, also do a case-aware lookup into `FileEntry` (not just exact-match).

### F15. `case_folds_equal` is ASCII-only — CONCERN
**Location:** `crates/onesync-core/src/engine/case_collision.rs:62-64`
**Severity:** CONCERN
**Summary:** APFS case-insensitivity is full Unicode (with NFC normalization), not ASCII. `eq_ignore_ascii_case` will treat `naïve.txt` vs `NAÏVE.txt` as distinct (the `ï` byte is non-ASCII). On real APFS volumes the OS folds them. So the engine's collision detection misses the very case it exists to handle.
**Suggested direction:** Use a proper Unicode case-fold (e.g. `unicode-case-mapping` crate or `caseless` crate) and apply NFC normalization first. The doc-comment already acknowledges this gap; promote it from "later if real-world filenames need it" to a near-term fix.

### F16. Reconcile path-derivation from delta uses `remote_item.name` as the full relative path — BUG
**Location:** `crates/onesync-core/src/engine/cycle.rs:181-185`
**Severity:** BUG
**Summary:** Delta items in OneDrive return only the leaf `name` plus a `parentReference`; the full path requires joining them. The code does `remote_item.name.parse::<RelPath>()` and treats the leaf name as the relative path. So `docs/notes.txt` and `archive/notes.txt` will collide as `notes.txt`, and any nested file is filed at the root.
**Evidence:**
```rust
let Ok(rel_path) = remote_item.name.parse::<RelPath>() else { continue; };
```
Cross-check with `RemoteItem::parent_reference` field present in the struct definition (reconcile tests construct `parent_reference: None`).
**Reasoning:** This breaks every test except the flat-root case. The integration tests probably pass because they only sync a single root with no nesting. Confirmed by skimming `engine_cycle_remote_dirty.rs`/`engine_cycle_local_upload.rs` (used flat paths in fixtures based on test path naming).
**Suggested direction:** Build the full path from `parent_reference.path` (which is shaped `/drive/root:/docs`) and the item name. Spec [04-onedrive-adapter.md] presumably documents the assembly.

### F17. Initial-sync 5-bullet rule (newer-mtime wins) not implemented — BUG (spec conformance)
**Location:** `crates/onesync-core/src/engine/cycle.rs:71-151`
**Severity:** BUG
**Summary:** Spec section "Initial sync" (lines 109-124) says for a path on both sides with differing content, "first-time-seen Dirty" → conflict policy (newer mtime wins). The code path that handles this is `reconcile_one` with `synced = None` — but that branch falls into `reconcile_both` only if `entry.is_some()` (i.e. the engine has an in-memory entry without a synced snapshot). On a true cold-start there is no `FileEntry`, so it hits `(None, Some(r)) → Download`. The local file is overwritten without checking content. **Initial-sync data loss.**
**Suggested direction:** Before `phase_delta_reconcile` decides on a brand-new remote item, scan locally for the same path and feed both observations to the conflict policy. The current code does this for local-first paths (the case-collision check there at least surfaces a duplicate-name case) but not for remote-first paths.

### F18. `phase_execute` does no actual concurrency — CONCERN
**Location:** `crates/onesync-core/src/engine/cycle.rs:481-539`
**Severity:** CONCERN
**Summary:** Despite `PAIR_CONCURRENT_TRANSFERS = 2` and `MAX_CONCURRENT_TRANSFERS = 4`, the execute phase is a serial `for mut op in ops { … .await }`. No semaphore, no `JoinSet`, no global concurrency permits. Spec section "Op execution" (line 222) requires bounded concurrency.
**Suggested direction:** Use a `tokio::sync::Semaphore` for the global cap and a per-pair `Semaphore::new(PAIR_CONCURRENT_TRANSFERS)`. Spawn ops onto a `JoinSet`. Be mindful of ordering: mkdirs must complete before child-creates (planner already buckets these; you can serialise per-bucket and parallelise within a bucket).

### F19. `phase_execute` always marks `SyncRun.outcome = Success` — BUG (observability)
**Location:** `crates/onesync-core/src/engine/cycle.rs:116-128`
**Severity:** BUG
**Summary:** `outcome: Some(RunOutcome::Success)` is unconditional, even when individual ops failed (F12) or `NotImplemented` (F7). Spec line 38-39 references a `PartialFailure` outcome (timeout case); the engine should classify the run by op-result counts.
**Suggested direction:** Track succeeded/failed counts; outcome = `Success` only if `failed == 0`, else `PartialFailure` (or `Failure` if all ops failed).

### F20. `pseudo_jitter` is deterministic in non-test contexts — CONCERN
**Location:** `crates/onesync-core/src/engine/cycle.rs:541-548`
**Severity:** CONCERN
**Summary:** "Production callers should supply true random jitter" — but the only caller in `phase_execute` uses `pseudo_jitter(attempt)`, so production *is* deterministic. Two failing pairs hitting the same backoff schedule will retry in lock-step, defeating the jitter's purpose (avoiding thundering-herd).
**Suggested direction:** Either inject a `Rng` port (best) or have the cycle accept a jitter closure. Don't ship a "production callers should…" comment shipped to production.

### F21. `RemoteDrive::download` returns the entire stream as bytes; executor copies it again — CONCERN
**Location:** `crates/onesync-core/src/engine/executor.rs:101-104`
**Severity:** CONCERN
**Summary:** `let write_stream = crate::ports::LocalWriteStream(stream.0.to_vec());` re-allocates the entire file in memory and then hands it to `local.write_atomic`. For a 10 GiB file (per `MAX_FILE_SIZE_BYTES`) this is two full copies in memory. The ports' doc-comment already acknowledges `LocalReadStream`/`LocalWriteStream` are "eager; streaming … later".
**Suggested direction:** Track this as TODO at the port level; do not let the eager shape leak into production assumptions.

### F22. `execute_download` uses `unwrap_or_default()` for `remote_item_id`, then sends "" — CONCERN
**Location:** `crates/onesync-core/src/engine/executor.rs:94-101`
**Severity:** CONCERN
**Summary:** Missing `remote_item_id` falls back to an empty string, which is then passed to `remote.download`. The adapter is then left to fail; the engine has no pre-flight check. Same pattern at `execute_upload` for `parent_remote_id` (line 119-124).
**Suggested direction:** Treat a missing metadata key as `ExecError::NotImplemented`/`Local(InvalidPath)` rather than relying on the adapter to error on an empty id — bad ids are an invariant violation, not a transient adapter issue.

### F23. Cycle holds no lock; concurrent triggers can race — CONCERN
**Location:** `crates/onesync-core/src/engine/cycle.rs`, scheduler stub
**Severity:** CONCERN
**Summary:** Spec phase 0 says: "Acquire pair lock (single-cycle-per-pair invariant)". `run_cycle` has no lock; `scheduler::spawn_pair_worker` is a stub. If anything in the daemon calls `run_cycle` twice concurrently for the same pair, the engine will happily run two cycles in parallel, two `op_insert`s with the same id collisions are avoided by ULIDs but `FileEntry.synced` updates (when added) will race.
**Suggested direction:** Either document that the lock is the caller's responsibility (and assert this via type signature — e.g. take a `&PairLock` parameter), or have `run_cycle` take a mutex-guard.

### F24. `find_case_collision` and `phase_delta_reconcile` use exact-path `FileEntry` lookup — BUG
**Location:** `crates/onesync-core/src/engine/cycle.rs:186-190, 265-269`
**Severity:** BUG
**Summary:** `state.file_entry_get(&pair, &rel_path)` is exact-byte-match keyed. On APFS the local file might be stored as `Foo.txt` while the delta returns `foo.txt`. Lookup misses; engine treats remote as new; downloads to `foo.txt`; APFS overwrites `Foo.txt`. Symmetric to F14; flagged independently because the fix lives at the StateStore boundary (would need a case-insensitive index in the schema) rather than at the engine.
**Suggested direction:** Either case-fold relative paths on insert (canonical-lowercase columns) or add a `file_entry_get_ci` port method.

### F25. Conflict policy "loser rename then propagate" is undocumented in code — CONCERN
**Location:** `crates/onesync-core/src/engine/cycle.rs`
**Severity:** CONCERN
**Summary:** Spec lines 194-208 specify an ordered 4-step group: rename loser, propagate rename to other side as new file, propagate winner content, insert conflict row. None of this is implemented; the only conflict path (`handle_case_collision`) does step 1+4 but skips 2 and 3. So even the partially-handled case-collision branch leaves the remote side ignorant of the renamed loser.
**Suggested direction:** Match the spec exactly: after `local.rename`, emit an `Upload` decision for `loser_rel` so the next cycle (or this cycle's plan phase) propagates it to remote. Currently the renamed file becomes "untracked" until the next cycle's local scan picks it up — slow, surprising, and easy to lose if a failure intervenes.

### F26. Scheduler is a stub; triggers, debouncing, backoff are not in this crate — CONCERN (scope)
**Location:** `crates/onesync-core/src/engine/scheduler.rs:79-89`
**Severity:** CONCERN
**Summary:** `spawn_pair_worker` returns a handle whose body just drains commands and exits on `Shutdown`. None of the five triggers (Scheduled, LocalEvent, RemoteWebhook, CliForce, BackoffRetry) are actually wired up to `run_cycle`. The two unit tests verify only that `mpsc::send` succeeds — they do **not** verify the worker calls anything.
**Reasoning:** This is "scope" because the daemon crate may be where this is wired. But every focus question about "do all paths converge on the same engine entrypoint" answers "there are no live paths to the engine entrypoint from this crate; the convergence assertion is unverifiable here." The risk is that a future PR wires each trigger differently.
**Suggested direction:** At minimum, define an integration-test fixture that calls `run_cycle` via a `WorkerCommand::Sync(Trigger::*)` to anchor the contract.

### F27. Several `unwrap_or_default()` calls in non-test engine code — NIT
**Location:**
- `cycle.rs:439-444` — `synthetic_remote_side`: `chrono::DateTime::from_timestamp(0, 0).unwrap_or_default()`
- `executor.rs:97-99, 122-124` — `metadata.get(...).unwrap_or_default()`
- `executor.rs:127-130` — `op.relative_path.as_str().rsplit('/').next().unwrap_or(...)` (this one is fine because `rsplit` always yields at least one element; flag only for review style)
**Severity:** NIT
**Summary:** Workspace bans `unwrap`/`expect`/`panic!` in non-test code; `unwrap_or_default`/`unwrap_or(...)` are not panics but they silently substitute potentially-wrong values. Several of these directly degrade safety (executor F22).

### F28. Reconcile branches on `entry.sync_state` but planner does not — CONCERN
**Location:** `crates/onesync-core/src/engine/reconcile.rs:65-73` vs `planner.rs:38-58`
**Severity:** CONCERN
**Summary:** Reconcile returns `NoOp` if the existing entry is `InFlight` or `PendingConflict`. But `phase_local_uploads` (`cycle.rs:271-277`) also gates on `InFlight`/`PendingConflict`. The planner does not consult `FileEntry.sync_state`. Logic that's repeated in two places will diverge.
**Suggested direction:** Centralise the "should we plan this?" check in one helper (e.g. `is_entry_quiescent(state)`) used by both phases.

### F29. `case_collision` insertion bypasses `FileEntry.synced` semantics — CONCERN
**Location:** `crates/onesync-core/src/engine/cycle.rs:326-405`
**Severity:** CONCERN
**Summary:** After `local.rename(from_abs, to_abs)`, the engine writes a `Conflict` row but does not delete or update the `FileEntry` at the **old** path. Next cycle will see the old `FileEntry` (still pointing at the now-non-existent local file at `Foo.txt`) plus the renamed file at `Foo (case-collision-xxx).txt` and emit a delete-then-upload sequence. That's a recovery, not data loss — but it generates churn and surprising audit noise.
**Suggested direction:** Update the old `FileEntry` to point at the loser path (or delete it and create a new one).

### F30. Spec says equality includes `kind`; reconcile checks only via `is_folder` toggles — CONCERN
**Location:** `crates/onesync-core/src/engine/reconcile.rs:46-53, 88-103`
**Severity:** CONCERN
**Summary:** Spec line 149: equality is `(kind, size_bytes, content_hash)`. `reconcile_both` never compares `entry.kind` against the remote's kind. A path that flipped from File to Directory (delete-and-recreate at remote) will be treated as a size-only change.
**Suggested direction:** Include kind in the equality predicate.

## Cross-cutting observations

- The engine has a clear cycle skeleton but is **structurally incomplete**: persistence of `delta_token`, `FileEntry.synced` post-op, retry-count durability, concurrency, locking, conflict-policy execution, and 4/8 executor branches are all stubbed or missing. The integration tests under `tests/` likely pass only because they exercise the narrow happy paths (flat-root remote-or-local, no conflict, no rename).
- **Pure-engine claim is upheld** on the I/O dimension — no `std::process`, no blocking IO, no direct file syscalls. Two style smells: `tokio::spawn` inside `scheduler::spawn_pair_worker` (the engine module ships a Tokio task) and `tokio::sync::mpsc` types in port DTOs (`LocalEventStream`) — these mean the engine has Tokio runtime dependence. Acceptable, but tightens the "no I/O" boundary; document it.
- **Limits hygiene** is good — every constant has a doc-comment, units in the identifier, no magic numbers in engine code that I spotted. `pseudo_jitter`'s constants 0.25 and 0.0 (`cycle.rs:546-547`) are the only inline literals, and they're justified by the function name.
- **Observability events** exist for `cycle.start`, `cycle.finish`, `op.failed`, `conflict.detected`, plus the case-collision rename event. Spec also requires `op.enqueued`, `op.started`, `op.finished`, `conflict.resolved.auto`, `conflict.resolved.manual`, `phase.timing` (line 297-303). None of these are emitted. So a cycle's audit trail is "started" + "finished" only — not enough to reconstruct decisions, contra spec line 297.
- **`unsafe` is forbidden** at crate level (`#![forbid(unsafe_code)]`) — confirmed clean.
- **No `panic!`/`unwrap`/`expect`/`todo!`/`unimplemented!`** in non-test code that I spotted (lints `clippy::unwrap_used`/`expect_used` allowed only in tests). Several `unwrap_or_default()` / `unwrap_or(...)` patterns are NITs called out in F27.

## What looks correct

- **`limits.rs`** — clean, single source of truth, named constants, doc-commented, with sanity tests at the bottom.
- **`retry::retry_decision`** — well-isolated, takes jitter as a parameter (testable), correctly bounded by `RETRY_MAX_ATTEMPTS`. Tests check ceiling and zero-jitter. The only catch is that the cycle wrapper doesn't use the returned `Backoff { delay_ms }` (F9).
- **`conflict::pick_winner_and_loser`** — pure, deterministic on inputs, no scan-order dependence. Tie-window logic is right. Tests cover both directions and tie-break. It just isn't called from `cycle.rs` (F4).
- **`reconcile::reconcile_one`** — the type signature is right (pure, takes data, returns `Decision`), and the dispatch table maps cleanly to the spec rows. Just the (true, true) → both-equal branch is missing (F1).
- **`types::Decision` / `DecisionKind`** — clear, every variant maps to a `FileOpKind` or `None`. Test of `to_file_op_kind` is locked in.
- **`case_collision::case_collision_rename_target`** — pure, deterministic (BLAKE3 of original path), preserves extensions, handles dotfiles and double-extensions correctly. The only gap is the ASCII-only fold (F15).
- **Port traits** — minimal, well-named, error enums distinguish retryable vs fatal categories cleanly (`GraphError::Throttled`, `Transient`, `Network` vs `Unauthorized`, `NotFound`, `Forbidden`). `Send + Sync` bounds are uniform. `StateStore` is intentionally a single fat trait per spec.
- **Tests in this crate are unit-level + 5 integration tests** — naming convention `engine_cycle_*` is consistent.

## Test-coverage observations (informational)

- `engine_cycle_conflict.rs` asserts `summary.conflicts_detected == 1` but does **not** verify a `Conflict` row was inserted, that any rename ran, or that any propagation happened. This is exactly the assertion shape that lets F4 slip through CI.
- Every integration test uses **flat root-level paths** (`"hello.txt"`, `"Report.pdf"`, `"shared.txt"`). The `FakeRemoteDrive::upload_sync` fixtures all pass `parent_reference: None`. This is the test shape that lets F16 (treating `remote_item.name` as the full path) slip through CI.
- Integration tests have an explicit `host_name: "testhost".to_owned()` but no test verifies the loser-rename string ever appears in the output — which is consistent with the spec-text mismatch in F13.


