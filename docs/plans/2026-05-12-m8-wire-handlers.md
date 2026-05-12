# onesync M8 — Wire stubbed handlers + close deferred carry-overs

> Inline execution (no subagent). All commits via `jj describe -m "..."` + `jj new`. Co-Authored-By: `Claude Opus 4.7 (1M context) <noreply@anthropic.com>` verbatim.

**Goal:** Replace the `not_implemented` stubs in `crates/onesync-daemon/src/methods/` with real implementations that call through to the `StateStore` / `engine` / adapters, close the two M4 integration tests that were deferred pending `DeltaPage` promotion, add the daemon `--check` self-validation mode, and update the roadmap to reflect what's actually working end-to-end.

**Scope contract:** Wiring only — no new features beyond what the spec already defines. Subscription streaming (real push semantics in `audit.tail` / `pair.subscribe` / `conflict.subscribe`) and the upgrade flow stay deferred; they need genuinely new infrastructure rather than method-body code.

Workspace test count: 271 entry → ≥ 290 exit.

---

## Pre-flight

- `origin/main` @ `1fe42e9e`. M1–M7 closed. 271 workspace tests pass.
- All method handlers other than `health.ping` / `health.diagnostics` return `MethodError::not_implemented`. `DispatchCtx` currently only carries `state: Arc<dyn StateStore>` + `started_at: Instant`. Will need to extend.

---

## Tasks

### Task 1: Extend `DispatchCtx` with the full port set

`DispatchCtx` becomes the engine-deps equivalent: `state`, `local`, `remote`, `vault`, `clock`, `ids`, `jitter`, `audit_sink`, plus the `started_at` already there. Wire all of those in `wiring.rs::build_ports` (they already exist on `DaemonPorts`).

Commit: `feat(daemon/methods): extend DispatchCtx with full port set`

### Task 2: `config.get` / `config.set` / `config.reload`

Smallest, single-table. `config.get` → `state.config_get`. `config.set` accepts a `Partial<InstanceConfig>` and merges. `config.reload` re-reads the row.

Commit: `feat(daemon/methods): config get/set/reload`

### Task 3: `account.list` / `account.get` / `account.remove`

Read paths first. `account.remove` honours `cascade_pairs: bool`; without cascade, refuse if any non-removed pair references this account.

Commit: `feat(daemon/methods): account list/get/remove`

### Task 4: `account.login.begin` / `account.login.await`

Spawn the loopback listener from `onesync-graph::auth::listener`, render the auth URL via `onesync-graph::auth::pkce` + `auth-code` URL builder, store the PKCE verifier + state behind a `login_handle` (ULID), return URL + handle. `login.await` blocks the handle's one-shot, exchanges the code, persists the `Account` row, returns it.

If the full flow proves heavier than expected, ship begin + await with the placeholder TokenSource (no real keychain write) and document the keychain wiring as a follow-up sub-task. Don't block M8 on getting the full OAuth dance through end-to-end.

Commit: `feat(daemon/methods): account login.begin and login.await`

### Task 5: Pair management — `add` / `list` / `get` / `pause` / `resume` / `remove`

- `pair.add`: validate `local_path` (must exist + writable), call `remote.item_by_path` to resolve remote, build `Pair`, upsert.
- `pair.list { account_id?, include_removed? }`: query `pairs_active` (or all pairs if `include_removed`).
- `pair.get`: `state.pair_get`.
- `pair.pause` / `pair.resume`: update `paused` flag.
- `pair.remove { delete_local, delete_remote, force? }`: soft-delete (set `status = Removed`); optionally invoke local/remote delete via adapters.

Commit: `feat(daemon/methods): pair add/list/get/pause/resume/remove`

### Task 6: `pair.force_sync` and `pair.status`

`pair.force_sync` returns immediately with a `SyncRunHandle { run_id, subscription_id }`. Spawns a `tokio::task` that calls `engine::run_cycle` with `RunTrigger::CliForce`. `subscription_id` is reserved for future progress streaming.

`pair.status` returns `PairStatusDetail { pair, in_flight_ops, recent_runs, conflict_count, queue_depth }`.

Commit: `feat(daemon/methods): pair.force_sync via engine::run_cycle and pair.status`

### Task 7: `conflict.list` / `conflict.get` / `conflict.resolve`

`list`: `state.conflicts_unresolved` (or all if `include_resolved`). `get`: look up by id (add a `conflict_get` query if not present in the state crate yet). `resolve { pick, keep_loser, note }`: update the conflict row (`resolved_at = now`, `resolution = "manual"`, optionally swap winner). Don't actually drive the rename — the engine handles that on the next cycle.

Commit: `feat(daemon/methods): conflict list/get/resolve`

### Task 8: `audit.search` / `run.list` / `run.get`

One-shot queries against the state store. `audit.tail` stays a stub (subscription wiring is M9+ — leave a clear `not_implemented` with rationale).

Commit: `feat(daemon/methods): audit.search, run.list, run.get`

### Task 9: `state.*` and `service.shutdown`

- `state.backup { to_path }`: use SQLite online-backup API via `rusqlite::backup::Backup`.
- `state.export { to_dir }`: dump every table as JSONL.
- `state.repair.permissions`: chmod 0700/0600 on state-dir contents; surface adjusted paths.
- `state.compact.now`: call `crate::retention::run`.
- `service.shutdown { drain }`: triggers the existing `ShutdownToken`.

Commit: `feat(daemon/methods): state backup/export/repair, service.shutdown`

### Task 10: Daemon `--check` self-validation mode

CLI flag `onesyncd --check`: open the state DB (verify migrations current), verify state-dir perms (0700), verify socket-dir writable, run lock acquisition test, exit 0 on success or matching exit code from the CLI table on failure.

Commit: `feat(daemon): --check self-validation mode`

### Task 11: Close the M4 deferred integration tests

Now that `DeltaPage` is populated (M5 Task 1 promoted it), write:

- `crates/onesync-core/tests/engine_cycle_remote_dirty.rs` — fake remote returns a delta with one changed item; engine plans a download; executor drives it.
- `crates/onesync-core/tests/engine_cycle_conflict.rs` — fake remote returns one item, local has a different file at the same path; engine plans rename-loser + upload; conflict row inserted.

The fakes need updating (in `onesync-graph::fakes::FakeRemoteDrive`) to allow injecting prepared delta items. Plumb that through the `NoopRemoteDrive` test helper as well.

Commit: `test(core/engine): integration — remote-dirty download + conflict cycle (closes M4 carry-over)`

### Task 12: M8 close

Run the workspace gate. Update the roadmap with a new "M8 — Handler wiring + carry-over close" section. Mark M4's "deferred" note resolved. Push.

Commit: `docs(plans): mark M8 complete; close M4 deferral`

---

## Self-review

- [ ] Every method previously returning `not_implemented` either has a real impl or is documented as a stub with rationale.
- [ ] `pair.force_sync` actually drives `engine::run_cycle`.
- [ ] `state.backup` produces a readable SQLite file.
- [ ] Daemon `--check` exits 0 on a healthy install, non-zero with the right code on a broken one.
- [ ] M4's two deferred integration tests pass.
- [ ] `cargo nextest run --workspace` >= 290 tests pass.
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` exits 0.
- [ ] `cargo fmt --all -- --check` exits 0.

## Out of scope (M9+ if ever)

- Subscription streaming (audit.tail / pair.subscribe / conflict.subscribe push semantics).
- Upgrade flow (`service.upgrade.prepare`/`commit` + binary swap).
- macOS host integration tests for the M7 lifecycle.
- Webhook receiver, SharePoint, case-sensitive APFS.
