# onesync M7 — Installation Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Workspace: `/Volumes/Delorean/onesync-m7-lifecycle/`. Commits via `jj describe -m "..."` + `jj new`. **Never invoke `git` directly.** Co-Authored-By trailer is verbatim `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.

**Goal:** Flesh out the `service` subcommands stubbed in M6 with the real macOS lifecycle: install/uninstall, start/stop/restart, doctor, upgrade flow. The end state: a fresh `onesync service install` on a macOS user account produces a running daemon, and `onesync service uninstall --purge` cleanly removes every file the install created (except synced user data).

**Architecture:** Lifecycle code lives inside `onesync-cli` (the binary the user invokes) and `onesync-daemon` (for the `--check` mode and `service.upgrade.*` RPCs already plumbed in M5). LaunchAgent plist generation is in the CLI. `launchctl` invocations are shell-outs (this is the supported public API to launchd).

**Tech Stack:** No new external Rust deps. `launchctl`, `open`, `id` are macOS system binaries. Plist generation via string templating (the plist format is small enough that a templating crate is overkill).

Workspace test count: M6 exit (≥ 320) → ≥ 335 exit (most M7 work is macOS-host integration tests under `#[ignore]` by default).

---

## Pre-flight

- M6 complete; `origin/main` is at M6's close commit.
- Workspace: `/Volumes/Delorean/onesync-m7-lifecycle/`.
- Spec: [`08-installation-and-lifecycle.md`](../spec/08-installation-and-lifecycle.md) is the authoritative reference for paths, plist contents, install sequence, upgrade flow, and uninstall behaviour.
- The `service` subcommands' clap surface already exists in M6 (stubs). M7 replaces the stub bodies.
- The daemon already accepts `service.upgrade.prepare` / `service.upgrade.commit` from M5. M7 wires the CLI side.

---

## File map

```
crates/onesync-cli/src/commands/service/
├── mod.rs                  # subcommand dispatch
├── install.rs              # service install
├── uninstall.rs            # service uninstall [--purge]
├── lifecycle.rs            # start / stop / restart
├── upgrade.rs              # service upgrade (full flow)
├── doctor.rs               # service doctor
└── plist.rs                # LaunchAgent plist template + rendering

crates/onesync-daemon/src/
├── self_check.rs           # --check mode
└── (additions to startup.rs for upgrade-detection)

crates/onesync-cli/tests/macos_e2e/
├── e2e_install.rs          # install → kickstart → ping → success
├── e2e_uninstall.rs
├── e2e_doctor.rs
└── e2e_upgrade.rs
```

10 tasks total. Workspace test count target: ≥ 335.

---

## Task 1: Plist generator

**Files:** `crates/onesync-cli/src/commands/service/plist.rs`

Render the LaunchAgent plist from the template in spec 08 with the actual user's home substituted. Use `dirs::home_dir()` (add `dirs = "5"` to workspace deps) or read `$HOME` directly.

```rust
pub struct PlistConfig {
    pub home: PathBuf,
    pub daemon_binary: PathBuf,
}

pub fn render(config: &PlistConfig) -> String;
pub fn write(config: &PlistConfig, target: &Path) -> Result<(), std::io::Error>;
```

**Tests:** unit tests asserting the rendered plist parses as valid XML via `plist` crate (or just regex-checks for `<key>Label</key>` + `<string>dev.onesync.daemon</string>`).

**Commit:** `feat(cli/service): LaunchAgent plist generator`

---

## Task 2: `service install`

**Files:** `crates/onesync-cli/src/commands/service/install.rs`

Per spec 08 §Install:
1. Refuse if `whoami` is `root`.
2. Copy bundled `onesyncd` binary to `~/Library/Application Support/onesync/bin/onesyncd`. The "bundled" location depends on distribution channel — for `cargo run`/Homebrew it's likely the same workspace's `target/release/onesyncd`. Make the source path configurable via `--from <path>` flag; default to "search for onesyncd alongside the running onesync binary".
3. Create `state-dir`, `runtime-dir`, `log-dir` with mode 0700.
4. Render plist to `~/Library/LaunchAgents/dev.onesync.daemon.plist`.
5. `launchctl bootstrap gui/$(id -u) <plist-path>` then `launchctl kickstart -k gui/$(id -u)/dev.onesync.daemon`.
6. Poll `health.ping` for up to `INSTALL_TIMEOUT_S` (60).
7. Print success including socket path and PID.

Idempotent: re-running rewrites plist, copies binary again (no-op if identical), `bootout` + `bootstrap`.

**Commit:** `feat(cli/service): install command`

---

## Task 3: `service uninstall [--purge]`

Per spec 08 §Uninstall:
1. Send `service.shutdown { drain: true }` to the running daemon; wait.
2. `launchctl bootout gui/$(id -u) <plist-path>`.
3. Remove the LaunchAgent plist.
4. Without `--purge`: keep state, logs, bundled binary.
5. With `--purge`: remove state-dir, log-dir, bundled binary, plus call `keychain` per-account delete via the daemon's `account.remove` RPC (which the daemon then calls `TokenVault::delete` for).

**Commit:** `feat(cli/service): uninstall command with --purge`

---

## Task 4: `service start / stop / restart`

- `start` — `launchctl kickstart gui/$(id -u)/dev.onesync.daemon`. Poll `health.ping`.
- `stop` — `service.shutdown { drain: true }` RPC. Wait for the daemon to exit (poll `health.ping` returning Err).
- `restart` — stop with drain, then start.

**Commit:** `feat(cli/service): start/stop/restart commands`

---

## Task 5: `service doctor`

Exercises every step from spec 08 §Health and self-checks:
- Plist exists.
- Plist parses.
- Daemon binary exists and is executable.
- State directory has correct permissions.
- Socket dir is writable.
- Daemon binary's `--check` mode (Task 7) passes.
- Daemon process is running (or report cleanly that it isn't).
- `health.ping` succeeds (only if daemon claimed-running).
- `health.diagnostics` returns sane values.

Output: a checklist with ✓/✗ per item. Exit code 0 if all green, 1 otherwise.

**Commit:** `feat(cli/service): doctor with full health checklist`

---

## Task 6: Upgrade flow

`onesync upgrade` (top-level command, also `onesync service upgrade`):
1. Locate the new `onesyncd` binary (Homebrew has updated `target/`; otherwise `--from <path>`).
2. Copy to `<state-dir>/bin/onesyncd.new`.
3. Send `service.upgrade.prepare` RPC; daemon drains in-flight ops for up to `UPGRADE_DRAIN_TIMEOUT_S` (30).
4. Send `service.upgrade.commit`; daemon flushes WAL and exits with code 75.
5. CLI renames `onesyncd.new` over `onesyncd`; keeps `onesyncd.<version>.bak` for one prior version.
6. `launchctl kickstart` to bring the new binary up.
7. Verify `health.ping` returns `schema_version >= previous`. If schema migrated forward, that's fine. If migrated backwards, CLI exits 1 — the user must restore from backup.

**Commit:** `feat(cli/service): upgrade flow with prepare/commit and binary swap`

---

## Task 7: Daemon `--check` mode

**Files:** `crates/onesync-daemon/src/self_check.rs`

When `onesyncd --check` is invoked, run all the offline validations:
- Migrations applied / current.
- State-dir perms (0700).
- Socket-dir writable.
- Lock file releasable (i.e. no live daemon claiming it; or the running daemon's PID matches expected).
- Plist (if present) has the expected `Label`.

Exit codes follow the CLI exit code table (7 for permissions, 78 for config, etc.).

Wire `--check` into `main.rs`'s arg parsing alongside `--launchd`.

**Commit:** `feat(daemon): --check self-validation mode`

---

## Task 8: macOS Full Disk Access surfacing

When FSEvents reports permission denied (per spec 08 §Permissions), surface a clear instruction to System Settings → Privacy & Security → Full Disk Access with the daemon binary's path. The instruction lands as a structured audit event `local.permission.denied` and a CLI message visible in `service doctor` output.

This is mostly UX — no new code path beyond ensuring the audit event payload includes the binary path.

**Commit:** `feat(daemon): FDA permission-denied surfacing with actionable hint`

---

## Task 9: macOS host integration tests

`crates/onesync-cli/tests/macos_e2e/`, all `#[ignore]` by default — they touch `launchctl` and real filesystem locations. Run explicitly with `cargo nextest run -p onesync-cli --run-ignored only`.

- `e2e_install.rs` — install, verify `launchctl list dev.onesync.daemon` shows the agent, ping the socket, uninstall, verify gone.
- `e2e_uninstall.rs` — install, uninstall without --purge, assert state directory survives; uninstall --purge, assert all files gone (except user data).
- `e2e_doctor.rs` — install, run doctor, expect all green; uninstall, run doctor on a fresh user, expect specific failures.
- `e2e_upgrade.rs` — install with version X; build a fake `onesyncd.new` that prints a marker; run upgrade; verify the new binary is running.

These tests must clean up after themselves: every `setUp` records every file it creates and tears down on `Drop`.

**Commit:** `test(cli): macOS host integration tests for service lifecycle (ignored by default)`

---

## Task 10: M7 close

- Run the full workspace gate.
- Update `docs/plans/2026-05-11-roadmap.md` M7 row with completion status. Mention the project is now end-to-end installable.
- Commit: `docs(plans): mark M7 complete on the roadmap`.

**Workspace test count target:** ≥ 335 (a few new unit tests for plist render + a few `#[ignore]`d integration tests).

---

## Self-review checklist

- [ ] `onesync service install` on a fresh user produces a running daemon within `INSTALL_TIMEOUT_S`.
- [ ] `onesync service uninstall --purge` removes every file the install created (state, logs, binary, plist, keychain).
- [ ] `onesync service doctor` reports an actionable diagnosis when each failure mode is triggered.
- [ ] `onesync upgrade` swaps the binary cleanly; rollback path leaves `onesyncd.<prev>.bak` for recovery.
- [ ] Refuses install when running as root.
- [ ] Plist parses as valid Apple plist XML.
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` exits 0.
- [ ] `cargo fmt --all -- --check` exits 0.

## Carry-overs and out-of-scope

- Homebrew formula and `.pkg` installer are distribution-channel work, not in scope for the repo itself. Spec 08 §Distribution methods documents them as future work.
- Apple Developer notarisation is also out of scope (no code changes; just signing).
- Webhook receiver remains an open question on spec 03 — does not block M7.

## Project complete

After M7 closes, the project ships an end-to-end macOS daemon + CLI that two-way-syncs against OneDrive (Personal or Business), per the original spec. Subsequent work would be distribution (Homebrew formula, .pkg notarisation), webhook receiver, SharePoint support, case-sensitive APFS volumes — all noted in the spec's open questions.
