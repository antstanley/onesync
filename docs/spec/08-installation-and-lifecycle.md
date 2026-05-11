# 08 — Installation and Lifecycle

**Status:** Draft · **Date:** 2026-05-11 · **Owner:** Stan

This page covers how onesync is installed on a macOS host, where its files live, how the
daemon is registered with `launchd`, how it starts and stops, how it is upgraded, and how
it is removed. The CLI subcommand `onesync service` is the user-facing surface for every
operation on this page.

---

## Files and paths

All paths follow per-user conventions on macOS. Onesync is a user service; it never installs
under `/Library/LaunchDaemons` or any system-wide directory.

| Purpose | Path |
|---|---|
| CLI binary | `/usr/local/bin/onesync` (Intel) or `/opt/homebrew/bin/onesync` (Apple Silicon, when via Homebrew). |
| Daemon binary | `~/Library/Application Support/onesync/bin/onesyncd` |
| LaunchAgent plist | `~/Library/LaunchAgents/dev.onesync.daemon.plist` |
| State directory | `~/Library/Application Support/onesync/` (mode 0700) |
| SQLite database | `~/Library/Application Support/onesync/onesync.sqlite` (0600) |
| IPC socket | `${TMPDIR}onesync/onesync.sock` (resolved from launchd's `TMPDIR`, 0600 dir + 0600 socket) |
| PID file | `${TMPDIR}onesync/onesyncd.pid` |
| Stdout log | `~/Library/Logs/onesync/onesyncd.out.log` |
| Stderr log | `~/Library/Logs/onesync/onesyncd.err.log` |
| Structured log (JSON Lines) | `~/Library/Logs/onesync/onesyncd.jsonl` |
| Lock file | `~/Library/Application Support/onesync/onesync.lock` |

`${TMPDIR}` resolved by `launchd` for a `LaunchAgent` is typically a path under
`/var/folders/...` and is per-user. We intentionally do **not** put the socket under
`~/Library` because `Library` paths are sometimes synced by other tools.

### Resolution order

The daemon resolves `<state-dir>`, `<runtime-dir>`, and `<log-dir>` at startup in this order:

1. Command-line flags (`--state-dir`, `--runtime-dir`, `--log-dir`) if present.
2. Environment variables `ONESYNC_STATE_DIR`, `ONESYNC_RUNTIME_DIR`, `ONESYNC_LOG_DIR` if set.
3. The macOS defaults listed in the table above.

Every directory is created on first start with `mode = 0700`.

---

## launchd integration

onesync runs as a per-user **LaunchAgent**, not a `LaunchDaemon`. LaunchAgents run in the
context of a logged-in user, do not run when no user is logged in, and have access to that
user's keychain. A LaunchDaemon would not have keychain access without involved workarounds
and is not appropriate for a per-user OneDrive sync.

The plist label is `dev.onesync.daemon`. Plist contents (verbatim shape, paths substituted at
install time):

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
                       "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>dev.onesync.daemon</string>

    <key>ProgramArguments</key>
    <array>
        <string>/Users/<user>/Library/Application Support/onesync/bin/onesyncd</string>
        <string>--launchd</string>
    </array>

    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key><false/>
        <key>Crashed</key><true/>
    </dict>

    <key>ThrottleInterval</key><integer>10</integer>
    <key>ProcessType</key><string>Background</string>
    <key>LowPriorityIO</key><true/>
    <key>Nice</key><integer>5</integer>

    <key>StandardOutPath</key>
    <string>/Users/<user>/Library/Logs/onesync/onesyncd.out.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/<user>/Library/Logs/onesync/onesyncd.err.log</string>

    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key><string>onesync=info</string>
    </dict>

    <key>SoftResourceLimits</key>
    <dict>
        <key>NumberOfFiles</key><integer>4096</integer>
    </dict>
</dict>
</plist>
```

`KeepAlive` is conditional: launchd restarts the daemon on crash but not on normal exit.
`SuccessfulExit = false` ensures a clean exit (e.g. via `onesync service stop`) is not
auto-restarted. `ThrottleInterval = 10` rate-limits restarts so a daemon that panics in a
loop will not eat CPU.

The `--launchd` argument switches the binary into the daemon mode that listens on the
socket; without it the binary refuses to start (this prevents accidental double-launch from
a terminal).

### Loading and unloading

`launchctl bootstrap gui/$(id -u) <plist>` registers the plist. `launchctl kickstart` starts
it. `launchctl bootout gui/$(id -u) <plist>` unloads. The CLI's `service` subcommands wrap
these calls with their preferred error handling and ensure the plist file is in place before
calling `bootstrap`.

`onesync service doctor` exercises every step: plist exists, plist parses, binary exists and
is executable, state directory permissions are correct, socket can be created, the daemon
binary's own self-check (`onesyncd --check`) passes.

---

## Install

```
onesync service install
```

The CLI:

1. Verifies that the calling user is not root. Onesync refuses to install for root; messages
   point the user at their user shell.
2. Copies the bundled `onesyncd` binary (the same binary built into the `onesync` CLI's
   package, located in a sibling resource directory) to
   `~/Library/Application Support/onesync/bin/onesyncd`.
3. Creates the state, runtime, log directories with `0700`.
4. Writes the LaunchAgent plist with the user's actual home path substituted.
5. `launchctl bootstrap` and `launchctl kickstart` the agent.
6. Polls `health.ping` until success or `INSTALL_TIMEOUT_S` (60).
7. Reports success including the socket path and PID.

The install is idempotent. Re-running on an already-installed system rewrites the plist,
copies the binary again (no-op if identical), and `bootout`+`bootstrap`s to pick up any
changes.

### Distribution methods

Two supported channels:

1. **Homebrew** (preferred):
   `brew install onesync`. The formula places `onesync` and a sibling resource directory
   containing `onesyncd`. The user then runs `onesync service install` to write the plist.
   The Homebrew install does not interact with launchd.
2. **Signed `.pkg`**: a notarised macOS installer that places the binaries and runs
   `onesync service install` for the current user as the post-install script. The package
   is per-user (the post-install reads the actual user via `$USER`, not root).

Notarisation is a [`docs/spec/00-overview.md` open question](00-overview.md#assumptions-and-open-questions).

---

## Upgrade

`onesync upgrade` (or a fresh `brew upgrade onesync`) follows this sequence:

1. Copy the new `onesyncd` binary to a sibling path (`onesyncd.new`).
2. Send the daemon a `service.upgrade.prepare` RPC, which finishes in-flight ops with a
   short deadline (`UPGRADE_DRAIN_TIMEOUT_S`, 30 seconds) and refuses new RPCs except
   `service.upgrade.commit`.
3. Send `service.upgrade.commit`. The daemon stops the IPC server, flushes the WAL, and
   exits with code 75 (`EX_TEMPFAIL`).
4. `launchd` does not restart on exit 75 — the CLI is responsible.
5. The CLI atomically renames `onesyncd.new` over `onesyncd`.
6. The CLI runs `launchctl kickstart` to bring the new binary up.
7. The CLI verifies `health.ping` returns a `schema_version` ≥ the previous value.

Schema migrations run automatically on the next start; the daemon refuses to come up if a
migration fails and surfaces the failure through `onesync service doctor`.

If the upgrade fails between steps 5 and 7, the old binary is still on disk as a backup
named `onesyncd.<previous-version>.bak`; the CLI's failure path restores it.

---

## Stop, start, restart

| Command | Behaviour |
|---|---|
| `onesync service stop` | RPC `service.shutdown { drain: bool }` then `launchctl kickstart -k` to confirm the agent is stopped. `drain` waits up to `SHUTDOWN_DRAIN_TIMEOUT_S` for in-flight ops; default is true. |
| `onesync service start` | `launchctl kickstart`. Polls `health.ping`. |
| `onesync service restart` | Stop with drain, then start. |

A graceful shutdown emits `service.shutdown.requested` audit, finishes the current cycles
per pair, persists state, closes the socket, and exits 0. Launchd does not auto-restart on
exit 0 (per `KeepAlive.SuccessfulExit = false`).

---

## Uninstall

```
onesync service uninstall [--purge]
```

1. Send `service.shutdown { drain: true }` and wait.
2. `launchctl bootout` the agent.
3. Remove the LaunchAgent plist.
4. Without `--purge`: keep state, logs, and the bundled binary in place so a re-install is
   resumable.
5. With `--purge`: also delete the state directory, log directory, and the bundled binary,
   and request the keychain adapter to remove every onesync entry (one per account).

Uninstalling never deletes the user's synced data. The local pair folders are user data and
are off-limits.

---

## Permissions and macOS prompts

On first FSEvents watch of a path under the user's home, macOS prompts for "Full Disk Access"
only if the watched root is in a protected location (Desktop, Documents, Downloads, iCloud
Drive). The CLI surfaces a clear instruction to grant access in System Settings → Privacy &
Security → Full Disk Access, listing the daemon's binary path.

The daemon detects denied access via FSEvents errors and surfaces them as
`local.permission.denied` with a hint pointing at the System Settings pane.

No other macOS permissions are required.

---

## Logging and rotation

Three log destinations, distinct concerns:

- `onesyncd.out.log` / `onesyncd.err.log`: launchd captures stdout/stderr verbatim. The
  daemon does not write to these in normal operation; they capture panic backtraces and
  early startup errors before the structured logger comes up.
- `onesyncd.jsonl`: the structured logger writes JSON Lines (one event per line). Rotated
  by the daemon itself: at `LOG_ROTATE_BYTES` (32 MiB) the daemon closes the file, renames
  it to `onesyncd.<timestamp>.jsonl`, and opens a fresh file. Old files past
  `LOG_RETAIN_FILES` (10) are deleted.
- `AuditEvent` rows in the SQLite database: the canonical, queryable log, retained per
  `AUDIT_RETENTION_DAYS`.

The CLI's `logs tail` follows the JSONL file (and falls back to subscribing to `audit.tail`
over IPC if the file is missing).

---

## Health and self-checks

The daemon exposes three self-check entry points:

- `onesyncd --check` (CLI flag): runs offline validation — db schema migrations are
  current, plist matches expected shape, permissions on state dir, socket dir writable —
  then exits with a code matching the [CLI exit codes](07-cli-and-ipc.md#cli-exit-codes).
  Used by `service doctor` and by CI smoke tests.
- `health.ping` RPC: liveness; cheap, no db access.
- `health.diagnostics` RPC: full snapshot (versions, pair states, recent runs, in-flight ops,
  pending conflicts, retention counts). Used by `service doctor` and bundled into support
  exports.

---

## Failure modes during lifecycle

| Symptom | Cause | Recovery |
|---|---|---|
| Daemon exits with 75 (`EX_TEMPFAIL`) | Upgrade flow | CLI swaps binary and restarts. |
| Daemon exits with 78 (`EX_CONFIG`) | Permission bits wrong, plist mismatch | `onesync service doctor` reports specifics. |
| Daemon exits with 70 (`EX_SOFTWARE`) | Internal bug; panic | Launchd restarts (KeepAlive on crash); audit captures backtrace; user is asked to file a bug. |
| Socket bind fails | Stale socket from a hard crash | Daemon unlinks the path if no live process holds it; otherwise refuses to start. |
| State db cannot be opened | Permissions or corruption | Daemon refuses to start; `state.repair.permissions` for perms; for corruption, restore from `state backup`. |
| Schema migration fails | Forward-incompatible db | Daemon refuses to start; the binary is too old for the db. CLI flags the version mismatch. |

---

## Assumptions and open questions

**Assumptions**

- The host is a single-user Mac or a shared Mac where each user runs their own daemon. There
  is no cross-user coordination of any kind.
- `launchctl` semantics on macOS 13+ are stable. `bootstrap`/`bootout` replaced the older
  `load`/`unload` and we standardise on the new commands.
- The user's home directory is on a local APFS volume. Network home directories are not
  supported.

**Decisions**

- *LaunchAgent, not LaunchDaemon.* **Per-user, keychain-accessible context.** A daemon
  running outside the user session would not have the user's keychain and would need
  Service Management APIs that complicate distribution.
- *`KeepAlive` on crash only.* **Clean exits are not restarted.** Avoids surprise re-launch
  when the user explicitly stops the service.
- *Logs rotated by the daemon, not by `newsyslog`/launchd.* **The daemon owns its rotation
  policy.** Keeps onesync portable to other macOS releases without touching system rotation
  config.
- *Single per-user instance.* **`fs2` advisory lock on `onesync.lock` enforces it.** Two
  daemons fighting over the same state would corrupt the file index.

**Open questions**

- *Distribution channel(s) at first release.* Homebrew is easy but does not handle
  notarisation guarantees; a `.pkg` adds a clean install UX but needs an Apple Developer
  account and notarisation pipeline. Possibly both, with Homebrew as the maintenance path.
- *Full Disk Access UX.* macOS gives no programmatic way to request FDA; we can only point
  the user at System Settings. Whether to bundle a small "first-launch" tutorial in the CLI
  is open.
