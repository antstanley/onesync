# onesync M9 — Connectivity + spec-decision implementations

> Draft. Execution method TBD (likely inline given M8's pattern). All commits via `jj describe -m "..."` + `jj new`. Co-Authored-By: `Claude Opus 4.7 (1M context) <noreply@anthropic.com>` verbatim.

**Goal:** Make onesync usable end-to-end against a real OneDrive account, and implement the directly-code-impacting spec decisions landed on 2026-05-12 (symlink audit emission, case-collision rename handling, Cloudflare-Tunnel webhook receiver, user-owned Azure AD client ID). After M9 a fresh `onesync service install` + `onesync account login` + `onesync pair add` should produce a working two-way sync without further hand-holding.

**Scope contract:** End-to-end connectivity + the four code-impacting decisions only. SharePoint (M11), distribution & notarisation (release-engineering milestone), subscription streaming + upgrade flow + macOS host integration tests (M10) all stay deferred per the M8/M9 split decided 2026-05-12.

Workspace test count: 277 entry → target ≥ 295 exit.

---

## Pre-flight

- `origin/main` @ `23609555` after the spec-creator checklist fixes. 277 workspace tests pass.
- M8 carry-overs documented in `crates/onesync-daemon/src/methods/{account,pair,state,service}.rs` rustdoc.
- `crates/onesync-core/src/engine/scheduler.rs` has `PairWorker` / `Trigger` / `WorkerCommand` scaffolding; the spawn-and-drive implementation is the M9 task.
- `crates/onesync-graph/src/auth/listener.rs` exists; the daemon-side state machine wrapping it does not.

---

## Tasks

### Task 1: Schema and protocol additions

Grow `InstanceConfig` and `Pair` with the three fields the new decisions require.

- `InstanceConfig.azure_ad_client_id: String` — required for OAuth begin.
- `InstanceConfig.webhook_listener_port: Option<u16>` — `None` disables the webhook receiver.
- `Pair.webhook_enabled: bool` — per-pair opt-in for `/subscriptions`.

Update `canonical-types.schema.json`, the Rust `serde` structs in `onesync-protocol`, and add SQLite migrations that backfill defaults (`""`, `NULL`, `false`).

Commit: `feat(protocol): add azure_ad_client_id, webhook_listener_port, webhook_enabled fields`

### Task 2: Wire `account.login.begin` / `account.login.await`

The daemon-owned PKCE state machine. `begin` reads `azure_ad_client_id` from `InstanceConfig` (refuses with a clear error if unset), spawns the loopback listener, builds the auth URL via the existing `auth::pkce` + `auth-code` helpers, stashes the verifier + state behind a ULID `login_handle`, and returns `{ login_handle, auth_url }`. `await` blocks the handle's one-shot, exchanges the code, fetches `/me` for the profile, persists the `Account` row + the keychain refresh-token entry. Uses the existing `onesync-keychain` adapter.

Track the in-flight logins in a `DashMap<LoginHandleId, LoginSession>` on `DispatchCtx` (or a dedicated `LoginRegistry` port — pick one in the implementation).

Commit: `feat(daemon/M9): wire account.login.begin and account.login.await`

### Task 3: Wire `pair.add`

Validate the local path via `LocalFs::path_exists` + `LocalFs::ensure_directory_exists`, refuse if outside the user's home directory or if it doesn't already exist, then call `remote.item_by_path(&drive_id, remote_path)` to resolve the remote root. Mint a `Pair` with a new ULID, default `webhook_enabled = false`, upsert. Emit `pair.added` audit event.

Commit: `feat(daemon/M9): wire pair.add with local + remote validation`

### Task 4: Daemon engine scheduler

A long-running tokio task spawned from `async_main`. Walks `state.pairs_list(None, false)` at `DELTA_POLL_INTERVAL_MS`, spawns a `PairWorker` per active pair on first sight, sends `Trigger::Scheduled` per tick. Drains on `ShutdownToken`.

Wire each `PairWorker` to call `engine::run_cycle` against the live `RemoteDrive` + `LocalFs` + `StateStore` ports. The pair's `delta_token` round-trips through the state store between cycles.

Commit: `feat(daemon/M9): engine scheduler — per-pair workers driving run_cycle`

### Task 5: Wire `pair.force_sync` and `pair.status`

`pair.force_sync` sends `Trigger::CliForce` to the named pair's `PairWorkerHandle`, returns `{ run_id, started_at }`. `pair.status` aggregates the Pair row + `runs_recent(pair, 5)` + `conflicts_unresolved(pair).len()` + in-flight ops (`file_entries_dirty(pair, MAX_QUEUE_DEPTH_PER_PAIR)`).

Commit: `feat(daemon/M9): wire pair.force_sync via scheduler and pair.status detail`

### Task 6: Wire `service.shutdown`

Thread `ShutdownToken` into `DispatchCtx`. Handler triggers the token; the existing top-level shutdown drain in `async_main` does the rest. Honour the `drain: bool` param (with `drain = false`, fire the token without waiting for outstanding cycles).

Commit: `feat(daemon/M9): wire service.shutdown via DispatchCtx-resident ShutdownToken`

### Task 7: Wire `state.*` maintenance handlers

- `state.backup { to_path }` → `rusqlite::backup::Backup::run`.
- `state.export { to_dir }` → JSONL dump per table.
- `state.repair.permissions` → chmod 0700 on state-dir, 0600 on its files; return the adjusted-path list.
- `state.compact.now` → invoke `crate::retention::run` (already present in `onesync-state`).

Commit: `feat(daemon/M9): wire state.backup / .export / .repair.permissions / .compact.now`

### Task 8: Symlink audit-event emission

Extend the `LocalFs::scan` callback set so the scanner can surface skipped entries (symlinks, `._*` resource forks, non-UTF8 names) with a reason code. The scheduler turns the symlink reason into a `local.symlink.skipped` audit event per pair (rate-limited to one per scan to avoid flooding).

Today `crates/onesync-fs-local/src/scan.rs:28` skips silently — the comment there literally says "Audit hook plumbed in Task 18", which never happened.

Commit: `feat(fs-local/M9): emit local.symlink.skipped audit events from scanner`

### Task 9: Case-collision rename handling

When the scanner observes two local entries whose NFC-normalised lowercase forms match (only possible on case-sensitive APFS volumes), pick the entry whose name matches the remote-side `relative_path` exactly as the canonical, rename the other to `<stem> (case-collision-<short-hash>)<.ext>`, and record a `Conflict` row with `resolution = None` so the operator sees it in `onesync conflicts list`. The renamed file flows through the normal pipeline as a new file.

The short-hash is the first 7 hex chars of the BLAKE3 of the original path bytes — deterministic so repeated scans don't keep renaming.

Commit: `feat(fs-local/M9): case-collision detection + rename per 01-domain-model decision`

### Task 10: Cloudflare-Tunnel webhook receiver

Daemon hosts an HTTP listener on `InstanceConfig.webhook_listener_port` (only when `Some`). Accepts Graph `/subscriptions` POST validations (responds with the `validationToken`) and notification deliveries (validates `clientState`, decodes the resource path, pushes `Trigger::RemoteWebhook` into the matching `PairWorker`).

The Graph adapter learns `subscribe(&drive_id, callback_url, &client_state)` and `unsubscribe(subscription_id)`. The scheduler subscribes for each pair where `webhook_enabled = true` on startup and unsubscribes on pair-remove or daemon shutdown.

Install docs gain a `cloudflared` config sample (in `docs/install/`). Polling stays the always-on fallback.

Commit: `feat(daemon/M9): Cloudflare-Tunnel webhook receiver + Graph /subscriptions plumbing`

### Task 11: Schema-compliance + migration tests

For each new field in Task 1: a schema-compliance test (fixture validates against `canonical-types.schema.json`) and a migration test that opens an M8-shape state DB and confirms the M9 migration runs and leaves rows readable with the new fields defaulted.

Commit: `test(state/M9): migration tests for InstanceConfig and Pair field additions`

### Task 12: End-to-end smoke test (#[ignore])

A `#[ignore]` integration test that, when run with `--ignored` and a real account credential in env, drives the full happy path: `account.login.begin` → manual paste of the auth code → `account.login.await` → `pair.add` → wait one scheduler tick → write a local file → assert it appears via `remote.item_by_path` → write a remote file via the test harness → assert it appears locally.

This test stays off by default because it touches a real Microsoft account; the runtime-derived `--ignored` flag opts in.

Commit: `test(daemon/M9): end-to-end happy-path smoke test (ignored by default)`

### Task 13: Update install docs

Add a section to `docs/install/` (creating it if absent) covering the Azure AD app-registration steps the user must complete before `account login` can work: app name, supported account types, redirect URI, required delegated scopes. Same section explains the optional `cloudflared` setup for webhooks.

Commit: `docs(install): Azure AD app registration + cloudflared tunnel setup`

### Task 14: Drop M8-deferred-stub rustdoc

Remove the "deferred" rustdoc blocks from the handlers that landed real impls in Tasks 2–7. Add module-level rustdoc summarising the now-supported flow.

Commit: `docs(daemon/M9): replace deferred-stub rustdoc with current-impl summaries`

### Task 15: M9 close

Run the workspace gate. Update the roadmap with the final M9 status block (commit SHAs, test counts, deferrals carried into M10). Push.

Commit: `docs(plans): mark M9 complete; carry remaining work into M10`

---

## Self-review checklist

- [ ] Every M8-documented stub that this milestone promises to wire has a real impl by Task 14.
- [ ] `azure_ad_client_id`, `webhook_listener_port`, `webhook_enabled` exist in code, schema, and the spec's body sections (no body section claims a field that doesn't exist).
- [ ] Migrations are additive and round-trip M8 state DBs without manual intervention.
- [ ] End-to-end smoke test runs successfully against a sandbox account.
- [ ] `cargo test --workspace --all-features` ≥ 295 tests pass.
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` exits 0.
- [ ] `cargo fmt --all -- --check` exits 0.

## Out of scope (carry-overs)

- SharePoint document libraries → **M11** (per decision in `04-onedrive-adapter.md`).
- Subscription streaming push (`audit.tail` / `pair.subscribe` / `conflict.subscribe`) → **M10**.
- Upgrade flow (`service.upgrade.prepare` / `service.upgrade.commit`) → **M10**.
- macOS host integration tests (`#[ignore]`d `launchctl`-driven e2e) → **M10**.
- Distribution + notarisation (Homebrew formula, `curl | bash` installer, signed binaries) → **release-engineering milestone, numbered when scheduled**.
- Adaptive `DELTA_POLL_INTERVAL_MS` → still in `03-sync-engine.md` open questions, awaiting soak data.
- Opt-in shallow symlink sync → still in `01-domain-model.md` open questions; no work committed.
