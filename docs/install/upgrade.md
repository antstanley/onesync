# Upgrading onesync

`onesync` supports in-place binary upgrade via the JSON-RPC IPC socket. The
two-phase flow lets you validate a staged binary before committing to a
restart, so you can catch path mistakes or permission issues without taking
the daemon down.

## Flow

1. **Stage the new binary** somewhere on disk. The recommended location is a
   sibling of the running binary, e.g. if the LaunchAgent runs
   `/usr/local/bin/onesyncd`, stage to `/usr/local/bin/onesyncd.next`.

2. **Validate via `service.upgrade.prepare`**. The daemon:
   - Verifies `binary_path` is absolute, exists, is a regular file, and has
     at least one executable bit set.
   - Stashes the path in process memory. No drain, no restart yet.

   ```sh
   onesync service upgrade prepare --binary-path /usr/local/bin/onesyncd.next
   # → { "ok": true, "staged_path": "/usr/local/bin/onesyncd.next" }
   ```

   On failure the daemon keeps running; fix the issue and call `prepare`
   again.

3. **Commit via `service.upgrade.commit`**. The daemon:
   - Triggers its shutdown token, which makes the scheduler stop dispatching
     new cycles and the IPC server close after the response is flushed.
   - In-flight sync cycles drain naturally (the scheduler only stops the
     accept loop; a cycle that is already running completes).
   - After the IPC server task joins, `main` reads the staged path and
     `execv`s into the new binary, keeping the same PID. macOS `launchd`
     sees the same PID alive and does not respawn the process.

   ```sh
   onesync service upgrade commit
   # → { "ok": true, "staged_path": "...", "message": "..." }
   ```

   The IPC response is sent before `exec`, so the command above returns
   normally.

## Failure modes

| Symptom | Cause | Fix |
|---|---|---|
| `commit` returns `APP_ERROR_BASE - 53` | No prepare in this process lifetime | Call `prepare` first |
| `prepare` returns `APP_ERROR_BASE - 50` | Path does not exist | Re-check the staged location |
| `prepare` returns `APP_ERROR_BASE - 52` | File is not executable | `chmod +x` |
| Daemon does not come back after commit | `exec` failed (logged); LaunchAgent will restart it from the *old* binary location next time it crashes | Inspect logs, fix the new binary, run `prepare` + `commit` again |

## Manual rollback

There is no automatic rollback. If the new binary misbehaves:

1. `launchctl bootout gui/<uid> /Library/LaunchAgents/io.onesync.daemon.plist`
2. Swap the binary back (`mv onesyncd.next onesyncd.broken; mv onesyncd.prev onesyncd`).
3. `launchctl bootstrap gui/<uid> /Library/LaunchAgents/io.onesync.daemon.plist`

A more polished upgrade pipeline (Homebrew tap + notarised binaries +
GitHub Releases) is tracked under the M13 release-engineering milestone.
