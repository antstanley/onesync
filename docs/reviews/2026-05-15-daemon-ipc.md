# Review: Daemon lifecycle & IPC (onesync-daemon) — 2026-05-15

## Scope
- `crates/onesync-daemon/src/lib.rs`, `main.rs`
- `crates/onesync-daemon/src/startup.rs`, `shutdown.rs`, `wiring.rs`, `lock.rs`, `logging.rs`, `check.rs`
- `crates/onesync-daemon/src/scheduler.rs` (daemon orchestration; distinct from `onesync-core/engine/scheduler.rs`)
- `crates/onesync-daemon/src/audit_sink.rs`, `login_registry.rs`, `webhook_receiver.rs`
- `crates/onesync-daemon/src/ipc/{mod,server,framing,dispatch,subscriptions}.rs`
- `crates/onesync-daemon/src/methods/{mod,account,audit,config,conflict,health,pair,run,service,state}.rs`
- `crates/onesync-daemon/tests/*.rs` (for contract expectations only)

## Method
Semi-formal certificate templates applied:
- **Patch verification / FUNCTION RESOLUTION** on RPC dispatch (`methods/mod.rs` route table → handler functions) to detect name shadowing and verify each advertised method name resolves to the intended handler.
- **Fault localization** on socket creation path and peer-cred check — divergence analysis: "Would tightening only one line close the trust gap?"
- **Execution trace** for shutdown drain — what tasks are awaited, which can outlive the lock release.

## Findings
(most severe first; F1, F2 …)

### F1. In-flight RPCs are not drained on shutdown — orphan connection tasks dropped with runtime — CONCERN
**Location:** `crates/onesync-daemon/src/ipc/server.rs:54-77`; `crates/onesync-daemon/src/main.rs:175-189`; `crates/onesync-daemon/src/methods/service.rs:1-37`
**Severity:** CONCERN (close to BUG — contract is misadvertised; see below)
**Summary:** The IPC server stops *accepting* on shutdown but never waits for already-accepted connections to finish dispatching. When `main` returns, the Tokio runtime is dropped and every `tokio::spawn(handle_connection(...))` is aborted mid-flight. The `service.shutdown` doc-comment ("the current shutdown path always drains") and the spec's drain semantics are not honoured.

**Evidence:**
- `ipc/server.rs:54-72`: accept loop with `tokio::select! { result = listener.accept(), _ = shutdown_rx.recv() => break }`. Each `Ok((stream, _))` spawns a detached `handle_connection` task with no handle stored.
- `ipc/server.rs:75-77`: on shutdown, returns immediately after `remove_file`. No `JoinSet` / no `tokio::time::timeout` waiting for connection tasks.
- `main.rs:181-189`: `let _ = rx.recv().await; match server_handle.await { … }` — only the *server task* is awaited, not connection workers. After that `async_main` returns, then `rt` (the multi-thread runtime) is dropped, which forcibly cancels every still-running task.
- `methods/service.rs:23-26`: comment claims "the current shutdown path always drains".

**Reasoning (execution trace):**
- Time `t0`: client A sends `pair.force_sync` over IPC. `handle_connection` reads the frame, calls `dispatch` which is `await`ing scheduler/store work.
- Time `t1`: client B sends `service.shutdown`. Handler calls `ctx.shutdown_token.trigger()` and returns `{"ok":true}`.
- Time `t2`: `ipc::server::run` select fires `_ = shutdown_rx.recv() => break`. Closes listener, removes socket file, returns.
- Time `t3`: `main` `server_handle.await` returns. `async_main` returns. `rt` is dropped. The `handle_connection` task for client A — still inside dispatch — is dropped (its future is cancelled). Client A sees connection reset with no response. No drain timeout was ever applied.
- Time `t4`: `_lock` dropped, exit.

**Suggested direction:** Track connection-task `JoinHandle`s in a `tokio::task::JoinSet` (or shared `Arc<Mutex<Vec<JoinHandle>>>`); after the accept loop breaks, await them with `tokio::time::timeout(IPC_DRAIN_TIMEOUT, …)`, then either let them finish or forcibly abort with a logged warning. Also have the per-connection read loop respect the shutdown token via `tokio::select!` so it stops reading new frames after shutdown, while finishing dispatches already in flight. Either fix the implementation to match the comment, or update the comment + spec to be honest about "best-effort, in-flight requests may be cancelled".

### F2. `subscription.cancel` is wired into dispatch but always returns `not_implemented` — contract gap — CONCERN
**Location:** `crates/onesync-daemon/src/methods/service.rs:123-125`; `crates/onesync-daemon/src/ipc/dispatch.rs:62`; `crates/onesync-daemon/src/ipc/subscriptions.rs:1-13`
**Severity:** CONCERN
**Summary:** The `SubscriptionRegistry` module docstring promises that `subscription.cancel` removes a subscription and stops the push stream, but the handler is a stub returning `MethodError::not_implemented("subscription.cancel")`. Clients have no documented way to stop a `pair.subscribe` / `conflict.subscribe` / `audit.tail` stream other than disconnecting.

**Evidence:**
- `ipc/subscriptions.rs:6-14`: doc says "client calls `subscription.cancel`" is the cancel path.
- `methods/service.rs:123-125`: stub returns `not_implemented`.
- `ipc/dispatch.rs:62`: name is still exposed → callers receive a successful `METHOD_NOT_FOUND`-equivalent application error rather than `-32601 method not found`.

**Suggested direction:** Either (a) hide `subscription.cancel` from the dispatch table until implemented (so clients get an unambiguous `METHOD_NOT_FOUND`), or (b) wire it through to `ConnCtx`'s subscription registry. Update the registry doc-comment if behaviour changes.

### F3. Socket peer-cred check intentionally absent — relies on directory perms that startup does not enforce — CONCERN
**Location:** `crates/onesync-daemon/src/ipc/server.rs:34-78`; `crates/onesync-daemon/src/startup.rs:49-55`
**Severity:** CONCERN
**Summary:** Spec `07-cli-and-ipc.md:299-302` explicitly leaves "Authentication on the socket" as an open question and relies on "0600 permissions plus per-user socket dir". The socket itself is `chmod 0600`, but `startup.rs::create_all` uses `std::fs::create_dir_all` without explicit mode, so the runtime dir inherits the process umask (typically 022 → 0755). On macOS the parent path is under `~/Library/Application Support/onesync/run/` whose ancestors are 0700, so the practical risk is low; but if the user runs with a permissive umask or sets `ONESYNC_RUNTIME_DIR=/tmp/foo`, any local user could open the socket and drive sync. There is no `getpeereid` / `SO_PEERCRED` check at accept time.

**Evidence:**
- `ipc/server.rs:43-48`: bind, then `set_permissions(0o600)`. Race: between bind and `set_permissions` the socket exists with default perms; a co-resident attacker process could `connect(2)` in that window.
- `ipc/server.rs:58-61`: accepted stream is passed to `handle_connection` with no peer-uid check.
- `startup.rs:49-55`: `create_dir_all` only — no explicit `set_permissions(0o700)` for `runtime_dir`.

**Reasoning (fault-localization template, DIVERGENCE):**
- Symptom: same-host non-owner process can drive the daemon under certain configurations.
- D1 (server.rs:43-48): bind→chmod race. Sufficient fix? No — also need parent-dir 0700.
- D2 (startup.rs:49-55): runtime_dir lacks explicit 0700. Sufficient fix? No — also need to close the bind→chmod race (e.g., set umask 077 around `bind`, or `bind` to tempname → `chmod` → `rename`).
- Together with an explicit peer-uid check at accept, all three close the trust gap.

**Suggested direction:** Three layered fixes:
1. `startup.rs`: after `create_dir_all`, explicitly `set_permissions(runtime_dir, 0o700)` and (defensively) `state_dir`, `log_dir`.
2. `ipc/server.rs`: set process umask to 0o077 around bind (or `bind` to a `.tmp` path, `chmod`, then `rename`), eliminating the race.
3. `ipc/server.rs`: after `accept`, call `getpeereid(2)` on the raw fd and compare to `geteuid()`; reject other uids. This is a defence-in-depth measure that survives operator misconfiguration of dir perms.

### F4. Webhook receiver treats `clientState` as a lookup key, not a shared secret — any localhost process can spam `force_sync` — HIGH (BUG-class trust model violation)
**Location:** `crates/onesync-daemon/src/webhook_receiver.rs:140-160,234-239`
**Severity:** CONCERN (HIGH on a multi-user / shared-tenant Mac; spec also flags this surface)
**Summary:** Microsoft Graph webhooks define `clientState` as a per-subscription **shared secret** that the receiver must verify equals the value it provided when registering the subscription. This implementation instead parses `clientState` as a `PairId` (`"pair_<ulid>"`) and uses it as the pair lookup key directly — no secret comparison. Anything that can reach `127.0.0.1:<webhook_port>` can forge a JSON body containing the ULID of any pair (ULIDs are leaked via `pair.list` etc.) and force the daemon to perform sync work. No HMAC validation, no IP allow-list beyond "localhost", no rate-limit.

**Evidence:**
- `webhook_receiver.rs:140-150`: parses notification body, extracts `clientState`, parses it as a `PairId`, calls `state.pair_get`, then `scheduler.force_sync(pair_id)` — bypasses the secret check entirely.
- `webhook_receiver.rs:234-239`: `parse_pair_id_from_client_state` is just `starts_with("pair_") && parse::<PairId>()`. Any local process can construct this.
- No rate-limiter, no auth header, no replay-window. The accept loop spawns a task per connection without backpressure → unbounded fan-out.

**Reasoning:**
- A pair ULID is enumerable: `onesync pair list` is exposed via `pair.list`, and the ULID is logged by the scheduler. Same-host malware that already has read access to the user's log dir (which is 0755 by default per F3) can recover pair_ids.
- `127.0.0.1` bind reduces blast radius to local processes, but on a multi-user Mac any user on the box can connect (TCP localhost is not user-scoped).

**Suggested direction:**
- Generate a per-subscription random `clientState` token at subscription-registration time, persist it alongside the pair, and constant-time-compare on every notification. Drop notifications whose token doesn't match.
- Add a small leaky-bucket rate limit per source IP, since the receiver is shared with cloudflared which can deliver bursts.
- Document the threat model in `04-onedrive-adapter.md` / `07-cli-and-ipc.md`.

### F5. Webhook receiver assumes the entire HTTP request fits in one `read()` — partial-read DoS — CONCERN
**Location:** `crates/onesync-daemon/src/webhook_receiver.rs:108-128`
**Severity:** CONCERN (NIT for cloudflared-only deployments, CONCERN otherwise)
**Summary:** A single `stream.read(&mut buf)` of 16 KiB is used to receive the full request. If the body or headers arrive in two syscalls (TCP segmentation, slow producer) `parse_request_head` returns "no header/body split" and the receiver rejects the request. Combined with the read-timeout, a malicious peer can simply pace bytes to keep one task pinned for `READ_TIMEOUT_S = 10` seconds. No connection cap → DoS (file descriptor exhaustion via `tokio::spawn`).

**Evidence:**
- `webhook_receiver.rs:113-120`: single `read` into 16 KiB buffer.
- `webhook_receiver.rs:114`: `READ_TIMEOUT_S = 10` per connection; no global concurrency cap.
- `webhook_receiver.rs:81-90`: every accept spawns an unbounded task.

**Suggested direction:** Use `tokio::io::BufReader` + a proper HTTP framing loop (read until `\r\n\r\n`, then `Content-Length` more), or just use `hyper` since it's already in the workspace's dep set. Add a `Semaphore` to cap concurrent webhook handlers.

### F6. `audit_sink` uses an unbounded mpsc — backpressure-free path to OOM under slow state store — CONCERN
**Location:** `crates/onesync-daemon/src/audit_sink.rs:34-50,52-72`
**Severity:** CONCERN
**Summary:** `DaemonAuditSink` uses `mpsc::unbounded_channel()`. The synchronous `AuditSink::emit` cannot block (it's called from inside the engine), so if `state.audit_append` is slow (SQLite WAL checkpoint, disk pressure, fsync stall) every cycle event keeps piling on the queue forever. There is no overrun signal back to the producer.

**Evidence:**
- `audit_sink.rs:36`: `let (tx, rx) = mpsc::unbounded_channel();` — no backpressure.
- `audit_sink.rs:60`: failed `audit_append` is logged via `tracing::error` but the event is dropped silently; no metric, no signal, no `audit.overrun` notification (which `subscriptions.rs` says SHOULD be emitted on overrun).
- `audit_sink.rs:69`: broadcast happens AFTER persist; live subscribers see "audit event" latency tied to SQLite throughput.

**Suggested direction:** Use a bounded channel sized to a few thousand events with a `try_send`; on overrun, drop oldest or coalesce, and emit an `audit.overrun` audit event. Consider broadcasting to subscribers BEFORE persisting so live tail keeps low latency.

### F7. Duplicate audit-event writes — `state.audit_append` plus `ctx.audit.emit` for the same event — CONCERN
**Location:** `crates/onesync-daemon/src/methods/account.rs:286-287`; `account.rs:478-479`; `pair.rs:180-181`; `audit_sink.rs:58-69`
**Severity:** CONCERN
**Summary:** Three method handlers (`account.login.await`, `account.add_sharepoint`, `pair.add`) write the same audit event to the state store twice: once directly via `state.audit_append(&evt)` and once via `ctx.audit.emit(evt)`. The `DaemonAuditSink::emit` task in turn calls `state.audit_append`. Because the event id is the same, the second insert likely violates the `UNIQUE` PK constraint, logs an error, and the event is preserved exactly once — but the log spam is misleading and any future audit table without a unique constraint would silently double-write.

**Evidence:**
- `pair.rs:180-181`:
  ```rust
  let _ = ctx.state.audit_append(&evt).await;
  ctx.audit.emit(evt);
  ```
- `audit_sink.rs:60`: `state.audit_append(&event).await` inside the drain task.
- `account.rs:286-287` and `account.rs:478-479`: identical pattern.

**Reasoning:** This appears to be a transitional artefact from before the drain task existed (when handlers had to persist themselves). Now that `DaemonAuditSink` owns persistence + broadcast, the explicit `state.audit_append` calls are dead — and harmful (log spam + extra DB round-trip).

**Suggested direction:** Delete the direct `state.audit_append` calls from `account.rs:286,478` and `pair.rs:180`. Audit events should flow through `ctx.audit.emit` exclusively.

### F8. `check.rs` does not exercise the IPC socket — `--check` claims more than it tests — CONCERN
**Location:** `crates/onesync-daemon/src/check.rs:60-198`; `crates/onesync-daemon/src/main.rs:58-82`
**Severity:** CONCERN
**Summary:** Brief asks: "does `--check` actually exercise … the socket". Answer: **no.** `--check` exits before reaching `ipc::server::run`, runs only `check_state_store`, `check_keychain`, `check_fsevents`, `check_full_disk_access`. There is no `check_socket` that binds the runtime dir, sets perms, and confirms a client could connect. Operator who runs `onesyncd --check` will not detect runtime-dir permission errors, stale socket files, or port collisions until the real daemon fails to start.

**Evidence:**
- `main.rs:63-70`: `--check` runs only those four probes.
- `check.rs` has no `check_socket` function; no module-level test for `IPC_KEEPALIVE` or `IPC_FRAME_MAX_BYTES`.
- Spec `08-installation-and-lifecycle.md:118`: "socket can be created" is part of the install validation, but it's not actually probed.
- Spec `08-installation-and-lifecycle.md:256`: lists "socket dir writable" as something `--check` should cover.

**Suggested direction:** Add `check::check_socket(runtime_dir)` that binds `<runtime_dir>/onesync-check.sock` (NOT the real socket file), sets `0600`, removes it. Confirms the runtime dir is writable and we can set perms. Wire it into the `--check` results array in `main.rs`.

### F9. `login_registry.rs` uses `expect("mutex poisoned")` — workspace bans `panic!`-equivalent — NIT
**Location:** `crates/onesync-daemon/src/login_registry.rs:44-49,55-61`
**Severity:** NIT (CONCERN if the workspace lint is strictly enforced)
**Summary:** `LoginRegistry::insert` and `LoginRegistry::take` call `.expect("login registry mutex poisoned")`. Every other module in this crate uses `unwrap_or_else(std::sync::PoisonError::into_inner)` (e.g. `methods/service.rs:88`, `main.rs:195`, `ipc/subscriptions.rs:78`). The inconsistent pattern means a poisoned mutex in the login registry will panic the entire daemon, even though the registry holds only oneshot receivers that can safely be observed post-poison.

**Evidence:** `login_registry.rs:47` and `:59` use `.expect(…)` with an `#[allow(clippy::expect_used)]`. Doc-comment justifies the panic as "unrecoverable", but other equally-critical state (subscription registry, upgrade staging path) recovers from poison.

**Suggested direction:** Switch to `unwrap_or_else(std::sync::PoisonError::into_inner)` for consistency with the rest of the crate. The lint allow + doc-comment justification is in tension with the spec rule banning panic.

### F10. `service.shutdown` returns success while drain is still in flight — client can't distinguish "drained" from "shutdown signalled" — NIT
**Location:** `crates/onesync-daemon/src/methods/service.rs:28-37`
**Severity:** NIT
**Summary:** `service.shutdown` returns `{"ok":true}` synchronously after `shutdown_token.trigger()`. The CLI cannot tell from the reply whether the daemon has actually drained or merely accepted the signal. Combined with F1 (no real drain), this gives operators false confidence.

**Suggested direction:** Either (a) await drain completion in-handler and reply afterwards, or (b) document the contract clearly: "ok=true means shutdown signalled, not drained — re-check with `health.ping` until the socket closes."

### F11. `health.diagnostics` returns hard-coded empty data — contract lies — CONCERN
**Location:** `crates/onesync-daemon/src/methods/health.rs:32-50`
**Severity:** CONCERN
**Summary:** `health.diagnostics` is documented as returning the full `Diagnostics` snapshot but in practice returns `pairs: []`, `accounts: []`, `config: null`, `subscriptions: 0_u32`. The doc-comment marks it as a Task-13 stub, but adjacent handlers (also from Task 13) are fully implemented. Operators running `onesync diagnostics` to triage a stuck daemon will receive misleading "everything is empty" responses.

**Evidence:** `health.rs:39-50` ignores `ctx.state` / `ctx.subscriptions` entirely.

**Suggested direction:** Populate `pairs`/`accounts`/`config` from `ctx.state.*`, populate `subscriptions` from `ctx.subscriptions.len()`. If a full impl is genuinely deferred, return `MethodError::not_implemented("health.diagnostics")` so callers can branch on it.

### F12. `state.repair.permissions` repairs only the state dir — runtime_dir + socket perms cannot be self-healed — NIT
**Location:** `crates/onesync-daemon/src/methods/state.rs:92-121`
**Severity:** NIT (CONCERN if combined with F3)
**Summary:** The handler walks only `ctx.state_dir`. It never touches `ctx.runtime_dir` (which holds the socket and lock file) or the log dir. If F3's permissive runtime_dir is the actual failure mode, the documented self-repair RPC can't fix it.

**Suggested direction:** Add a sibling method `service.repair.runtime_permissions` that chmod 0700 the runtime dir and re-chmods 0600 the socket and 0600 the lock file. Either expose it via a flag on `state.repair.permissions` or rename to `state.repair_permissions` and document the broader scope.

### F13. Webhook delivery is unbounded — accept loop never throttles — NIT (related to F5)
**Location:** `crates/onesync-daemon/src/webhook_receiver.rs:80-103`
**Severity:** NIT
**Summary:** `tokio::spawn` per accepted TCP connection with no upper bound. A misconfigured cloudflared, a misbehaving Graph notifier, or a same-host attacker can starve the daemon of fds before any rate-limiter even decides.

**Suggested direction:** Wrap the accept loop with a `tokio::sync::Semaphore` (e.g. 32 permits); decline new connections when saturated.

### F14. Log rotation: dead branch in the shift loop — NIT
**Location:** `crates/onesync-daemon/src/logging.rs:99-113`
**Severity:** NIT
**Summary:** `for n in (1..=LOG_RETAIN_FILES).rev()` cannot satisfy `if n > LOG_RETAIN_FILES`, so the "remove the oldest rotated file" branch is unreachable. On Unix the subsequent `rename` overwrites the destination silently, so the *effect* is correct, but the dead branch is confusing.

**Suggested direction:** Either iterate `(1..=LOG_RETAIN_FILES + 1).rev()` (so the oldest slot triggers the removal) or delete the unreachable arm and rely on `rename(2)`'s overwrite semantics. Add a doc comment explaining the chosen behaviour.

### F15. `wiring::build_ports` is also called by `--check` — runs every migration on a freshly-resolved state dir without ever cleaning up — NIT
**Location:** `crates/onesync-daemon/src/check.rs:60-68`; `crates/onesync-daemon/src/wiring.rs:59-93`
**Severity:** NIT
**Summary:** `check_state_store` calls `wiring::build_ports`, which creates the SQLite file and runs migrations. After the probe `DaemonPorts` is dropped, but the `onesync.sqlite` file remains. If the operator runs `--check` against a clean prod-style state dir, they will leave behind an empty database that the next daemon start treats as "already initialised". Probably benign because migrations are idempotent, but it makes `--check` a side-effecting probe — the module's own doc says "Probes are side-effect-free".

**Suggested direction:** Either redirect the probe to a temp dir (and copy migrations into it), or document `--check` as a one-way operation that initialises state on first run.

### F16. `audit.tail` registers in a process-global registry — cross-connection isolation only via fan-out filtering — NIT
**Location:** `crates/onesync-daemon/src/methods/audit.rs:23-40`; `crates/onesync-daemon/src/ipc/subscriptions.rs:91-111`
**Severity:** NIT
**Summary:** Every `audit.tail` subscriber on every connection enters the same `SubscriptionRegistry`, which `broadcast`s to *all* of them on every event. When channel `try_send` returns `Full`, the warning "subscription channel full — event dropped" is logged once per dropped event but no `audit.overrun` event is emitted to either the affected subscriber or the persistent audit log. Slow consumer behaviour is "drop oldest silently" which contradicts the spec's promise that overruns are observable.

**Suggested direction:** When a `try_send` fails with `Full`, emit a synthetic `audit.subscription.overrun` notification on the *same* channel (after draining) and persist it via the regular path. Optionally disconnect the slow subscriber after N consecutive overruns.

### F17. `account.login.begin` derives `login_handle` from `state_token` — both round-trippable to the OAuth state — NIT
**Location:** `crates/onesync-daemon/src/methods/account.rs:87-88`
**Severity:** NIT
**Summary:** `state_token = ctx.ids.new_id::<AccountTag>().to_string()` is followed by `login_handle = format!("lgn_{}", state_token.trim_start_matches("acct_"))`. The two values share a ULID, so anyone who sees the login_handle (returned to the client and logged) can reconstruct the OAuth `state` parameter. State is already in the auth URL (which the user opens in a browser), so leakage is low impact, but it complicates audit-log redaction — a tail of `audit.login_begin` would correlate to live OAuth states.

**Suggested direction:** Use two independent ULIDs: one for the state, one for the handle. Cheap fix.

### F18. Webhook receiver: HTTP `Content-Length: 0` with body parses as zero-length and accepts — NIT
**Location:** `crates/onesync-daemon/src/webhook_receiver.rs:199-222`
**Severity:** NIT
**Summary:** `body_from_buffer` defaults `content_length` to `0` if the header is absent, then `body[..content_length].to_owned()` returns `""`, which `serde_json::from_str` rejects as parse-error. Cleanly returns an error; not exploitable, but the failure mode is "empty notification payload" rather than the more honest "missing Content-Length".

**Suggested direction:** Require `Content-Length` for non-validation POSTs and return 411 Length Required otherwise.

## Cross-cutting observations

- **Drain contract is asserted but not exercised by tests.** `tests/shutdown_drain.rs` checks the server task exits and the socket file disappears; it never spawns an in-flight RPC across the shutdown boundary. Adding a test that sends `pair.force_sync` → trigger shutdown → assert the response arrives before connection close would surface F1 immediately.
- **`#[allow(dead_code)]` is used liberally** (`shutdown.rs:11`, `audit_sink.rs:13`, `ipc/mod.rs:6`, `wiring.rs:23,58`). Each justification cites a future task, but most of those tasks now appear complete. The allows should be re-evaluated; some likely flag genuinely dead code post-Task-13.
- **Process lock is `fs2` flock on `runtime/onesync.lock`** (lock.rs). No PID file, no stale-lock recovery beyond `WouldBlock → AlreadyRunning`. If `onesyncd` crashes hard the kernel releases the flock so a clean restart works. Race between `onesyncd --check` and `onesyncd start`: `--check` skips the lock (main.rs:58-82), so `--check` cannot detect "another daemon is running" — operator validation doesn't notice a live conflicting daemon.
- **Method dispatch FUNCTION RESOLUTION certificate**: every name in `dispatch.rs:17-66` resolves to one path-qualified `methods::<module>::<fn>` call. No Rust-builtin shadowing (no `Self::add`, `Self::list` etc. — handlers are free functions on modules). No trait-dispatch ambiguity. Dispatch is correct by construction; the only wart is `subscription.cancel` returning `not_implemented` (F2).
- **Exit-code convention**: `main.rs` returns `anyhow::Result<()>` so the exit code is 0 on graceful shutdown, non-zero on early errors (lock contention, dir create failure). `--check` exits with `i32::from(any fail)` (0 or 1). launchd `KeepAlive { SuccessfulExit = false }` will restart on non-zero only — that's compatible. `THROTTLE_INTERVAL` (default 10s) protects against tight loops. Looks OK; an explicit "fatal vs restartable" code split would help.
- **`unsafe` is forbidden** (`#![forbid(unsafe_code)]` at top of `lib.rs` and `main.rs`). No `unsafe` blocks found. ✓
- **`panic!`/`todo!`/`unimplemented!`**: none in main code paths. The closest are `login_registry.rs` `.expect("mutex poisoned")` calls (F9). Some `.unwrap_or_default()` on `serde_json::to_string` (e.g. `main.rs:79`) — these only fail on serialisation bugs and degrade gracefully.

## What looks correct

- **Socket cleanup**: `ipc::server::run` removes a stale socket file before binding (server.rs:38-42) and again on graceful shutdown (line 75). Combined with `fs2` advisory lock on `onesync.lock`, two daemons cannot accidentally race.
- **JSON-RPC framing**: `read_frame` enforces `IPC_FRAME_MAX_BYTES` cap (framing.rs:40-42), strips both `\n` and `\r\n` (lines 44-49), distinguishes `Closed` (`n==0`) from `TooLarge`. Single `read_line` plus `BufReader` means partial reads across syscall boundaries are handled correctly. Embedded literal newlines inside string values are rejected because `read_line` stops at the first `\n` (which the parser would then see as a truncated frame and return a parse error response) — this matches the spec's "embedded newlines not permitted" rule.
- **Per-connection writer multiplex**: `writer_task` in `server.rs:141-178` interleaves responses + subscription notifications onto one write half. Use of `tokio::select!` randomisation prevents one channel starving the other. The drain-tail logic on `response_rx` closure (lines 158-167) is a thoughtful touch.
- **Subscription GC**: `subscriptions::spawn_gc` sweeps closed senders periodically (subscriptions.rs:139-149) and `broadcast` itself removes closed entries on every push (lines 99-110). No leak even if a subscriber crashes.
- **Lock acquisition** uses `fs2::FileExt::try_lock_exclusive` correctly; `WouldBlock` → `AlreadyRunning` mapping is precise (lock.rs:45-52). Kernel releases on process death so stale-lock cleanup is automatic.
- **Shutdown token** is a small, clear primitive (`shutdown.rs`); broadcast channel with capacity 1 + idempotent trigger via `let _ = self.tx.send(())` is the right shape. Signal handler covers SIGTERM + SIGINT, returns immediately if registration fails. ✓
- **Scheduler shutdown ordering**: `scheduler.rs:157-167` reacts to the shutdown signal AND best-effort unregisters Graph subscriptions before exiting. ✓ (However the scheduler `JoinHandle` is not awaited from `main` — see F1.)
- **`pair.subscribe`/`conflict.subscribe` filter logic** correctly walks both `params.pair_id` and `params.payload.pair_id` shapes so it works with both raw `AuditEvent` serialisations and any future wrapper.

## Severity counts
- BUG: 0
- CONCERN: 11 (F1, F2, F3, F4, F5, F6, F7, F8, F10, F11, F12)
- NIT: 7 (F9, F13, F14, F15, F16, F17, F18)

Top finding: **F4** — webhook receiver uses `clientState = pair.id` as a lookup key instead of a per-subscription shared secret. Any local process that can reach the webhook port can spam `force_sync` on any pair whose ULID it can guess or enumerate.





