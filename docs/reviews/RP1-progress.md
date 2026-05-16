# RP1 (sync engine) — remediation progress

**Last update:** 2026-05-16
**Head:** main at `0675c31f`
**Plan:** `docs/plans/2026-05-15-remediation.md`
**Source review:** `docs/reviews/2026-05-15-sync-engine.md`

## Status

**All 14 RP1 BUGs closed.** All three BUG-follow-ons (F4 op-group, F14 auto-rename, F24 auto-rename) landed. **9 of 13 CONCERNs + 1 of 2 NITs landed** in the post-BUG batch.

Full-workspace verification gates clean:

- `cargo fmt --all -- --check` — PASS
- `cargo run -p xtask -- check-schema` — PASS
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — PASS
- `cargo nextest run --workspace` — 351 passed, 5 skipped (baseline +39 new tests, zero regressions)

## CONCERN / NIT progress

| F | Title | Status | Commit |
|---|---|---|---|
| F8 | Retry counter not persisted across cycles | ✅ done — `op_update_status` carries attempts | `0675c31f` |
| F9 | Retry loop ignores backoff delay | ✅ done — `tokio::time::sleep` + `Throttled.retry_after_s` | `611c38f6` |
| F12 | State-store error halts entire cycle | ✅ done — per-op isolation | `2b768a46` |
| F15 | Case-fold ASCII-only | ✅ done — NFC + Unicode lowercase | `58049f0f` |
| F18 | No actual concurrency in `phase_execute` | ⏳ deferred (large structural — semaphores + JoinSet) | — |
| F20 | Deterministic `pseudo_jitter` in production | ✅ done — `getrandom`-based real jitter | `c39d5583` |
| F21 | `RemoteDrive::download` eager `Vec<u8>` | ⏳ deferred (port streaming refactor) | — |
| F22 | Missing op metadata → silent empty string | ⚠ partial — `InvalidOp` for rename/mkdir/delete; upload/download wait on planner metadata population | `fa6dca71` |
| F23 | No pair lock — concurrent triggers race | ⏳ deferred (large structural — type-system enforcement) | — |
| F25 | Conflict steps 2+3 wholly unwired | ⚠ partial — winner=Remote auto-resolves via the F4 follow-on; winner=Local still defers | (F4 follow-on `ddd670f6`) |
| F26 | Scheduler stub; no live trigger paths | ⏳ deferred (daemon-side wiring, RP5 territory) | — |
| F28 | Quiescence check duplicated | ✅ done — `reconcile::is_action_blocking` centralised | `ed71d553` |
| F29 | Case-collision bypasses `FileEntry.synced` semantics | ✅ done — clears `local` + transitions to PendingConflict | `b2ac8120` |
| F30 | Equality predicate omits `kind` | ✅ done — kind flip caught in `remote_differs_from_synced` | `ce8a6f4d` |
| F27 | `unwrap_or_default` cleanups (NIT) | ⚠ partial — `synthetic_remote_side` + 4 executor sites cleaned; 2 remain on `execute_download`/`execute_upload` (couples with F22) | `fa6dca71` |
| F13 | Conflict-filename spec mismatch (NIT-tilting-BUG) | ✅ done in BUG tier | `8c9ab3b4` |

## Remaining at the end of RP1

Five items remain — three are structural enough to deserve their own commit-set, two are partials that close once their structural dependency lands:

- **F18 concurrency.** `phase_execute` is serial. Spec wants bounded concurrency (`PAIR_CONCURRENT_TRANSFERS`, `MAX_CONCURRENT_TRANSFERS`). Needs `tokio::sync::Semaphore` for global + per-pair caps and a `JoinSet` for parallel op execution, with care around mkdir-before-create ordering. Substantial refactor.
- **F21 download streaming.** `RemoteDrive::download` returns `RemoteReadStream(Vec<u8>)` — full file in memory. For 10 GiB files (the `MAX_FILE_SIZE_BYTES` ceiling) this is multiple full copies during the executor's `Vec.to_vec()` step. Port shape needs an actual `AsyncRead`-style stream type.
- **F23 pair lock.** Spec phase 0 requires "Acquire pair lock (single-cycle-per-pair invariant)". Currently no lock. Needs a type-encoded `PairLock` parameter on `run_cycle` (or a `Mutex` in the scheduler crate).
- **F25 winner=Local auto-resolve.** The F4 follow-on closed winner=Remote auto-resolution. winner=Local needs `RemoteRename` + `Upload`, and the Upload needs `parent_remote_id` — which the planner doesn't yet populate. Couples with the same planner-metadata work F22 waits on.
- **F26 scheduler wiring.** The scheduler in `onesync-core` is a stub; the live trigger paths (Scheduled, LocalEvent, RemoteWebhook, CliForce, BackoffRetry) live in `onesync-daemon`. Properly an RP5 concern.

Plus the small **F22 / F27 partial**: cleaning up `execute_upload`/`execute_download`'s `unwrap_or_default` cases. This unblocks once the planner populates `parent_remote_id` / `remote_item_id` metadata on the ops it emits — same prereq as F25 winner=Local and the broader F22 close. The right shape is probably a `populate_op_metadata` post-planner pass in `run_cycle` that reads `FileEntry` + `Pair` to fill metadata.

The five deferred items + the small partial are all flagged with their RP-prefix or `// TODO RP1-Fxx` comments inline. None are blocking for RP2.

| F | Title | Status | Commit |
|---|---|---|---|
| F1 | Reconcile both-converged NoOp branch | ✅ done | `8c7e26c3` |
| F2 | Remote etag-fallback to size silently absorbs same-size edit | ✅ done (paired with F6) | `3548693d` |
| F3 | Conflict winner/loser placeholder values | ✅ done | `f281a737` |
| F4 | Planner drops Conflict — silent divergence | ✅ minimal `3af4de43`; winner=Remote auto-resolve (rename + download) `ddd670f6`. winner=Local still parks at PendingConflict pending `parent_remote_id` lookup. |
| F5 | Local divergence uses mtime; ignores content | ✅ done | `a7aa91c1` |
| F6 | Remote-divergence size-only fallback (sibling of F2) | ✅ done with F2 | `3548693d` |
| F7 | Executor only 4 of 8 op kinds | ✅ done | `1717b251` |
| F10 | Cycle never persists new delta cursor / FileEntry.remote | ✅ done | `c3d1cdb1` |
| F11 | FileEntry.synced not updated after a successful op | ✅ done | `dae3519d` |
| F13 | Conflict loser filename missing detection timestamp | ✅ done | `8c9ab3b4` |
| F14 | Case-collision symmetric (remote-only collision pair) | ✅ detection + audit `0f7ef3c7`; auto-rename of losers `725d32e2`. |
| F16 | Delta `name` treated as full path | ✅ done | `04f03db8` |
| F17 | Initial-sync 5-bullet rule not implemented | ✅ done | `7e05c6d5` |
| F19 | SyncRun.outcome always Success | ✅ done | `601a4518` |
| F24 | `file_entry_get` is byte-exact; misses case-insensitive collisions | ✅ port extension + detection + audit `bb51a526`; auto-rename of colliding delta items `eff51259`. |

## What's structurally fixed across all 14 BUGs

- **Cycle state-loop closed end-to-end.** Delta-page items persist to `FileEntry.remote`; `Pair.delta_token` advances after persistence; post-op success updates `FileEntry.synced` + `sync_state` (Clean); `SyncRun.outcome` is classified from actual op counts.
- **Reconcile decision tree spec-accurate.** `(true,true)` splits into NoOp (provably equal) vs ConflictDetected (everything else); remote-divergence requires positive equality evidence (etag, both-zero-byte), never falls back to size-only; local divergence uses `(kind, size, content_hash)` per spec — `mtime` no longer consulted.
- **Executor covers all 8 op kinds.** `NotImplemented` is unreachable through `execute()`.
- **Delta-item paths assembled from `parent_reference` + `name`.** Nested paths no longer collide as flat-root leaves.
- **Content conflicts surface and park.** `phase_resolve_conflicts` inserts a `Conflict` row and transitions the `FileEntry` to `PendingConflict` so subsequent cycles short-circuit. Operators see the conflict via `onesync conflicts list` and can resolve manually.
- **Initial-sync collisions handled.** A path with both local and remote content at first-time encounter promotes the Download decision to ConflictDetected and attaches the local side to the FileEntry.
- **Remote case-collisions detected.** Two delta items that case-fold equal pick a deterministic canonical and drop losers with an audit event.
- **Case-insensitive `FileEntry` lookup exists at the port.** A byte-exact miss followed by a CI hit emits an audit event and skips the delta item — no silent overwrite via APFS-folded download.

## Follow-on status

All three deferred follow-ons from the BUG-tier checkpoint are now landed:

- **F4 follow-on (winner=Remote).** `record_content_conflict` emits a `LocalRename` + `Download` pair: the loser is moved to a disambiguated path locally and the remote winner content lands at the original path. The FileEntry auto-resolves to `Clean`. winner=Local stays at `PendingConflict` — auto-resolving that direction requires a parent-remote-id lookup (`Upload`'s `parent_remote_id` metadata is currently empty in the planner) which is its own work item.
- **F14 auto-rename.** `process_remote_case_collisions` builds rename targets via `case_collision_rename_target` for each loser; `build_remote_rename_ops` emits `RemoteRename` ops appended after the planner's output. Both colliding files end up with distinct names on remote.
- **F24 auto-rename.** `skip_for_case_collision` now also pushes a rename target for the colliding delta item. The existing local FileEntry survives at its original case; the remote arrival is renamed.

All three rely on a shared `metadata.from_conflict = true` flag on the emitted ops, and `update_file_entry_post_op` short-circuits when it sees that flag — keeps the related FileEntry untouched (so PendingConflict / Clean transitions stay coherent with the surrounding logic).

## What's next in RP1 — 13 CONCERNs + 2 NITs

Notable CONCERNs from `docs/reviews/2026-05-15-sync-engine.md`:

- **F8** retry counter not persisted across cycles.
- **F9** retry loop ignores backoff delay (must honour `retry_after_s` from `Throttled`).
- **F12** state-store error halts the entire cycle (should isolate per-op).
- **F15** case-fold is ASCII-only (Unicode required for real APFS filenames).
- **F18** no actual concurrency in `phase_execute`.
- **F20** `pseudo_jitter` deterministic in production.
- **F21** `RemoteDrive::download` returns full bytes; `LocalWriteStream` re-allocates.
- **F22** missing op metadata keys treated as silent empty strings.
- **F23** no pair lock — concurrent triggers race.
- **F25** conflict-policy step 2+3 still unwired (couples with F4 follow-on).
- **F26** scheduler is a stub; triggers / debouncing / backoff not in this crate.
- **F28** quiescence check duplicated across `reconcile` + `phase_local_uploads`.
- **F29** case-collision insertion bypasses `FileEntry.synced` semantics.
- **F30** equality predicate omits `kind`.

NITs: F13 (already done), F27 (`unwrap_or_default` cleanups in executor + cycle).

## Then RP2 — RP7

Per `docs/plans/2026-05-15-remediation.md`, after full RP1 closure:
- **RP2** Graph I/O (14 findings; includes the upload-session resume math bug surfaced first in the original review)
- **RP3** CLI + protocol + keychain + time (34 findings)
- **RP4** Local FS (20 findings; F1 there now consumes the richer port error variants that RP1 implicitly relies on through audits)
- **RP5** Daemon + IPC (18 findings)
- **RP6** Auth / OAuth (11 findings)
- **RP7** State store (11 findings)

## Notes for the next session

- All changes live on `main`; `jj log -r main..` from `b4b89b04` (baseline) shows the 14 RP1 BUG commits + 2 docs commits in order. Bisect-friendly.
- The workspace gate at `bb51a526` is the new pre-CONCERNs reference point.
- Daemon-side scheduler still calls into the engine through a `CycleCtx`; nothing in the daemon changed during RP1 BUGs, so RP5 hasn't been pre-touched.
- `FakeRemoteDrive` still sets `parent_reference: None` on all items. RP1-F16 (delta path assembly) is unit-tested only because of this; an integration test that exercises nested paths needs the fake updated. Filed mentally for RP2 (Graph I/O).
- The conflict policy's `pick_winner_and_loser` is now wired (via `record_content_conflict`); the previous "dead code" smell from the review is resolved.
- `cargo nextest run --workspace` runs in ~2 s once compiled (347 tests). Cold compile is ~3 min on this machine; keep that in mind for CI estimates.
