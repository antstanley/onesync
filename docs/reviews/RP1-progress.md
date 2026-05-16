# RP1 (sync engine) — remediation progress

**Last update:** 2026-05-16
**Head:** main at `bb51a526`
**Plan:** `docs/plans/2026-05-15-remediation.md`
**Source review:** `docs/reviews/2026-05-15-sync-engine.md`

## Status

**All 14 RP1 BUGs closed.** Full-workspace verification gates clean:

- `cargo fmt --all -- --check` — PASS
- `cargo run -p xtask -- check-schema` — PASS
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` — PASS
- `cargo nextest run --workspace` — 347 passed, 5 skipped (baseline +35 new tests, zero regressions)

RP1 CONCERNs (13) and NITs (2) remain.

| F | Title | Status | Commit |
|---|---|---|---|
| F1 | Reconcile both-converged NoOp branch | ✅ done | `8c7e26c3` |
| F2 | Remote etag-fallback to size silently absorbs same-size edit | ✅ done (paired with F6) | `3548693d` |
| F3 | Conflict winner/loser placeholder values | ✅ done | `f281a737` |
| F4 | Planner drops Conflict — silent divergence | ⚠ minimal: Conflict row + PendingConflict transition; full 4-step op group (rename loser + propagate rename + propagate winner) deferred | `3af4de43` |
| F5 | Local divergence uses mtime; ignores content | ✅ done | `a7aa91c1` |
| F6 | Remote-divergence size-only fallback (sibling of F2) | ✅ done with F2 | `3548693d` |
| F7 | Executor only 4 of 8 op kinds | ✅ done | `1717b251` |
| F10 | Cycle never persists new delta cursor / FileEntry.remote | ✅ done | `c3d1cdb1` |
| F11 | FileEntry.synced not updated after a successful op | ✅ done | `dae3519d` |
| F13 | Conflict loser filename missing detection timestamp | ✅ done | `8c9ab3b4` |
| F14 | Case-collision symmetric (remote-only collision pair) | ✅ done (detection + audit; auto-rename deferred) | `0f7ef3c7` |
| F16 | Delta `name` treated as full path | ✅ done | `04f03db8` |
| F17 | Initial-sync 5-bullet rule not implemented | ✅ done | `7e05c6d5` |
| F19 | SyncRun.outcome always Success | ✅ done | `601a4518` |
| F24 | `file_entry_get` is byte-exact; misses case-insensitive collisions | ✅ done (port extension + detection + audit; auto-rename deferred) | `bb51a526` |

## What's structurally fixed across all 14 BUGs

- **Cycle state-loop closed end-to-end.** Delta-page items persist to `FileEntry.remote`; `Pair.delta_token` advances after persistence; post-op success updates `FileEntry.synced` + `sync_state` (Clean); `SyncRun.outcome` is classified from actual op counts.
- **Reconcile decision tree spec-accurate.** `(true,true)` splits into NoOp (provably equal) vs ConflictDetected (everything else); remote-divergence requires positive equality evidence (etag, both-zero-byte), never falls back to size-only; local divergence uses `(kind, size, content_hash)` per spec — `mtime` no longer consulted.
- **Executor covers all 8 op kinds.** `NotImplemented` is unreachable through `execute()`.
- **Delta-item paths assembled from `parent_reference` + `name`.** Nested paths no longer collide as flat-root leaves.
- **Content conflicts surface and park.** `phase_resolve_conflicts` inserts a `Conflict` row and transitions the `FileEntry` to `PendingConflict` so subsequent cycles short-circuit. Operators see the conflict via `onesync conflicts list` and can resolve manually.
- **Initial-sync collisions handled.** A path with both local and remote content at first-time encounter promotes the Download decision to ConflictDetected and attaches the local side to the FileEntry.
- **Remote case-collisions detected.** Two delta items that case-fold equal pick a deterministic canonical and drop losers with an audit event.
- **Case-insensitive `FileEntry` lookup exists at the port.** A byte-exact miss followed by a CI hit emits an audit event and skips the delta item — no silent overwrite via APFS-folded download.

## Deferred follow-ups (still part of RP1 logical scope; not BUGs anymore)

- **F4 full op group.** The minimal F4 records the conflict and parks the entry. The spec's 4-step propagation (rename loser → propagate rename → propagate winner → record) is not yet emitted. Operators currently must resolve manually.
- **F14 auto-rename.** Remote case-collisions are detected and audited; the loser is not yet renamed remotely.
- **F24 auto-resolution.** Case-collision lookup hits emit an audit and skip; they don't promote to a Conflict row or auto-rename.

All three follow-ups are extensions of working code paths — none of them require new structural decisions.

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
