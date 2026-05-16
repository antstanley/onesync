# RP1 (sync engine) — remediation progress

**Last update:** 2026-05-16
**Head:** main at `3af4de43`
**Plan:** `docs/plans/2026-05-15-remediation.md`
**Source review:** `docs/reviews/2026-05-15-sync-engine.md`

## Status

11 of 14 RP1 BUGs closed. All workspace gates remain green at `main`.

| F | Title | Status | Commit |
|---|---|---|---|
| F1 | Reconcile both-converged NoOp branch | ✅ done | `8c7e26c3` |
| F2 | Remote etag-fallback to size silently absorbs same-size edit | ✅ done (paired with F6) | `3548693d` |
| F3 | Conflict winner/loser placeholder values | ✅ done | `f281a737` |
| F4 | Planner drops Conflict — silent divergence | ⚠ minimal: Conflict row + PendingConflict transition committed; full 4-step op group (rename loser + propagate rename + propagate winner) deferred | `3af4de43` |
| F5 | Local divergence uses mtime; ignores content | ✅ done | `a7aa91c1` |
| F6 | Remote-divergence size-only fallback (sibling of F2) | ✅ done with F2 | `3548693d` |
| F7 | Executor only 4 of 8 op kinds | ✅ done | `1717b251` |
| F10 | Cycle never persists new delta cursor / FileEntry.remote | ✅ done | `c3d1cdb1` |
| F11 | FileEntry.synced not updated after a successful op | ✅ done | `dae3519d` |
| F13 | Conflict loser filename missing detection timestamp | ✅ done | `8c9ab3b4` |
| F14 | Case-collision symmetric (remote-only collision pair) | ⏳ pending | — |
| F16 | Delta `name` treated as full path | ✅ done | `04f03db8` |
| F17 | Initial-sync 5-bullet rule not implemented | ⏳ pending | — |
| F19 | SyncRun.outcome always Success | ✅ done | `601a4518` |
| F24 | `file_entry_get` is byte-exact; misses case-insensitive collisions | ⏳ pending | — |

## What's structurally fixed

- Cycle state-loop is closed end-to-end: delta-page items persist to `FileEntry.remote`, `Pair.delta_token` advances after persistence, post-op success updates `FileEntry.synced` + `sync_state` (Clean), and `SyncRun.outcome` is classified from actual op counts.
- Reconcile decision tree is spec-accurate: (true,true) splits into NoOp (provably equal) vs ConflictDetected (everything else); remote-divergence requires positive equality evidence (etag, both-zero-byte), never falls back to size-only.
- Local divergence uses `(kind, size, content_hash)` per spec; `mtime` is no longer consulted as an equality signal.
- Executor dispatches all 8 `FileOpKind` variants. `NotImplemented` is no longer reachable through `execute()`.
- Delta-page items have their full relative path assembled from `parent_reference.path` + `name` (the leaf-name-only bug is closed).
- Content conflicts now materialise a persisted `Conflict` row and transition the `FileEntry` to `PendingConflict`, ending the silent-divergence loop. The Conflict row is `resolution = None` so operators see it and can resolve manually via `onesync conflicts resolve`.

## What's still outstanding in RP1 BUGs

### F4 follow-on — full conflict op group

The current minimal F4 records the conflict + parks the entry but does not execute the spec's 4-step propagation (rename loser on its own side → propagate rename to the other side → propagate winner content → record). Operators must currently resolve manually.

Next step: extend `record_content_conflict` to emit the rename + transfer ops:
- Winner=Local → `RemoteRename` (item → loser leaf), then `Upload` (local → original remote).
- Winner=Remote → `LocalRename` (original → loser path), then `Download` (remote → original local).

Step 2 (propagate rename across sides as a new file) is what makes the loser visible on both sides. The simplest path is to rely on subsequent cycles to pick up the renamed file via local scan / delta — which the engine already does post-F10/F11.

### F14 — case-collision symmetry

`find_case_collision` only fires from the local-scan side. Trace asymmetric cases:
- Remote has both `Foo.txt` and `foo.txt`, local sees only one (APFS folds).
- Local has `Foo.txt`, remote renames to `foo.txt`.

Fix direction (from review): bucket remote items by `case_folds_equal` in `phase_delta_reconcile`; emit a `ConflictDetected` (or new `RemoteCaseCollision` decision) when a bucket has >1 entry. Also case-fold `FileEntry` lookups (couples with F24).

### F17 — initial-sync cross-side observation

When a brand-new remote item appears in `phase_delta_reconcile` and the same path exists locally without a prior `FileEntry`, the engine emits `Download` and overwrites the local file. Spec's initial-sync rule says: feed both observations to the conflict policy first.

Fix direction: in `phase_delta_reconcile`, before resolving `(None, Some(remote))` as Download, peek the local FS for that path. If both exist with differing content, emit `ConflictDetected` instead.

### F24 — case-insensitive FileEntry lookup

`state.file_entry_get(&pair, &rel_path)` is exact-byte-match. APFS stores `Foo.txt` but remote delta returns `foo.txt` → lookup misses, engine treats remote as new, downloads to `foo.txt`, APFS coalesces with `Foo.txt`, original content lost.

Fix direction:
- Add `StateStore::file_entry_get_ci` (or store canonical-lowercase columns).
- Update `phase_delta_reconcile` and `phase_local_uploads` to use the CI lookup where APFS-folded collision is possible.

This crosses the port-trait boundary — touches `onesync-core/ports/state.rs`, `onesync-state` (SQLite impl + index), and the in-memory `fakes.rs`. Plan to land as part of RP1 because the engine is the consumer.

## After all RP1 BUGs

13 CONCERNs + 2 NITs remain in `docs/reviews/2026-05-15-sync-engine.md`. Notable:
- **F8** retry counter not persisted across cycles.
- **F9** retry loop ignores backoff delay (must honour `retry_after_s` from Throttled).
- **F12** state-store error halts entire cycle (should isolate per-op).
- **F18** no actual concurrency in `phase_execute`.
- **F22** missing metadata keys treated as silent empty strings.
- **F23** no pair lock — concurrent triggers race.

## Then RP2 — RP7

Per `docs/plans/2026-05-15-remediation.md`, after RP1's full closure:
- RP2 Graph I/O (14 findings, including the upload-session resume math bug)
- RP3 CLI + protocol + keychain + time (34 findings)
- RP4 Local FS (20 findings; F1 depends on richer port error variants from RP1)
- RP5 Daemon + IPC (18 findings)
- RP6 Auth / OAuth (11 findings)
- RP7 State store (11 findings)

## Notes for the next session

- All changes so far live on `main`; nothing in branches. `jj log` shows the 11 RP1 commits cleanly.
- The baseline at `b4b89b04` is the pre-RP1 reference for any "did I break this?" question.
- Workspace lints stay strict (pedantic + nursery + `-D warnings`). Several iterations on each commit were spent satisfying clippy — keep that in the budget.
- `cargo nextest run -p onesync-core` is fast (~3-5s once compiled). Full-workspace `cargo nextest run --workspace` was last green at baseline; have not re-run it after each commit to save build time. Worth doing before RP1 closure.
- The fakes' `FakeRemoteDrive` sets `parent_reference: None` on all items, so the F16 path-assembly fix has no integration-test coverage — only the unit tests in `cycle.rs::tests`. RP2 Graph I/O may want to update the fakes to populate parent_reference correctly, which would also strengthen RP1 coverage retroactively.
