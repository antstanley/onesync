# onesync M5 — Daemon + IPC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Workspace: `/Volumes/Delorean/onesync-m5-daemon/`. All commits via `jj describe -m "..."` + `jj new`. **Never invoke `git` directly.** Co-Authored-By trailer is verbatim `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` on every commit.

**Goal:** Build `onesync-daemon` (binary `onesyncd`) — long-running per-user process that wires the engine to the adapters, hosts a JSON-RPC 2.0 server on a Unix domain socket, multiplexes subscriptions, logs structured events, handles signals, and exits cleanly. Plus a small port-shape fix carried over from M4 (`DeltaPage` promotion).

**Architecture:** Single new crate `crates/onesync-daemon/`. Composes every existing crate; depends on Tokio's multi-threaded runtime. The IPC surface is the contract defined in [`docs/spec/07-cli-and-ipc.md`](../spec/07-cli-and-ipc.md).

**Tech Stack:** `tokio` (already pinned, multi-thread runtime), `serde`/`serde_json` for JSON-RPC framing, `tracing` + `tracing-subscriber` for structured logs, `fs2` for the advisory lock, `signal-hook-tokio` for SIGTERM/SIGINT. No new HTTP deps (Unix socket only). `MAX_RUNTIME_WORKERS` lands here in `limits.rs`.

VCS: jj-colocated. Workspace test count: 255 entry → ≥ 290 exit.

---

## Pre-flight

- M1–M4 complete; `origin/main` @ `e76e2f39`. 255 workspace tests pass.
- Workspace `/Volumes/Delorean/onesync-m5-daemon/` (jj workspace name `onesync-m5-daemon`).
- Spec pages: [`07-cli-and-ipc.md`](../spec/07-cli-and-ipc.md), [`02-architecture.md`](../spec/02-architecture.md) §Concurrency model, [`08-installation-and-lifecycle.md`](../spec/08-installation-and-lifecycle.md) §Files and paths (use the path resolution table).
- The carried-over `DeltaPage` placeholder is fixed in Task 1 (this milestone needs to surface remote items to the engine before any RPC method is meaningful).

---

## File map

```
crates/onesync-daemon/
├── Cargo.toml
└── src/
    ├── main.rs                 # entry, arg parse, runtime setup, signal wiring
    ├── startup.rs              # path resolution, dir creation, advisory lock
    ├── paths.rs                # state-dir / runtime-dir / log-dir resolution
    ├── lock.rs                 # fs2 advisory lock helper
    ├── wiring.rs               # build all ports (StateStore, LocalFs, RemoteDrive, …)
    ├── logging.rs              # tracing-subscriber + JSONL rotation
    ├── shutdown.rs             # graceful shutdown sequence
    ├── ipc/
    │   ├── mod.rs
    │   ├── server.rs           # Unix socket listener + connection accept loop
    │   ├── framing.rs          # line-delimited JSON encoder/decoder
    │   ├── dispatch.rs         # JSON-RPC method dispatch
    │   ├── subscriptions.rs    # subscription multiplexer + GC
    │   └── version.rs          # major-version handshake
    ├── methods/
    │   ├── mod.rs
    │   ├── health.rs           # health.ping / health.diagnostics
    │   ├── config.rs           # config.get / config.set / config.reload
    │   ├── account.rs          # account.login.begin/await / list / get / remove
    │   ├── pair.rs             # pair.add / list / get / pause / resume / remove / force_sync / status / subscribe
    │   ├── conflict.rs         # conflict.list / get / resolve / subscribe
    │   ├── audit.rs            # audit.tail / search
    │   ├── run.rs              # run.list / run.get
    │   ├── state.rs            # state.backup / export / repair.permissions / compact.now
    │   └── service.rs          # service.shutdown / upgrade.prepare / upgrade.commit
    └── error.rs                # internal error → RpcError mapping
```

16 tasks total. Workspace test target: ≥ 290.

---

## Task 1: `DeltaPage` promotion (port-shape carry-over)

**Files:**
- Modify: `crates/onesync-core/src/ports/remote_drive.rs` — promote `pub struct DeltaPage;` and `pub struct RemoteItem;` to populated types.
- Modify: `crates/onesync-graph/src/adapter.rs` — populate the real `DeltaPage` from Graph responses.
- Modify: `crates/onesync-graph/src/fakes.rs` — populate `DeltaPage` from the fake's internal map.
- Modify: `crates/onesync-core/src/engine/cycle.rs::collect_remote_entries` — extract items from `DeltaPage` and convert to `(RelPath, FileSide)`.

```rust
// onesync-core::ports::remote_drive

pub struct RemoteItem {
    pub id: onesync_protocol::primitives::DriveItemId,
    pub name: String,
    pub size_bytes: u64,
    pub mtime: onesync_protocol::primitives::Timestamp,
    pub etag: Option<onesync_protocol::primitives::ETag>,
    /// Path relative to the pair's remote root, slash-separated.
    pub relative_path: onesync_protocol::path::RelPath,
    pub kind: onesync_protocol::enums::FileKind,
    /// If true, this item is a tombstone (deleted on the remote).
    pub deleted: bool,
    /// BLAKE3 not provided by Graph; populate from sha1/quickXor on download if needed.
    pub content_hash: Option<onesync_protocol::primitives::ContentHash>,
}

pub struct DeltaPage {
    pub items: Vec<RemoteItem>,
    pub next_cursor: Option<onesync_protocol::primitives::DeltaCursor>,
}
```

After this change, `collect_remote_entries` returns real items, and the deferred M4 tests (dirty-remote-download, conflict cycle) become writable.

**Step 1.1:** Promote the types.
**Step 1.2:** Update `onesync-graph::adapter` to populate `DeltaPage` from the wire response.
**Step 1.3:** Update `onesync-graph::fakes::FakeRemoteDrive` to populate `DeltaPage` from its internal map.
**Step 1.4:** Update `collect_remote_entries` in `cycle.rs` to map `RemoteItem` → `(RelPath, FileSide)`.
**Step 1.5:** Update the in-engine integration tests' `NoopRemoteDrive` to return an empty populated `DeltaPage` (`items: vec![], next_cursor: None`).
**Step 1.6:** Add a new integration test `engine_cycle_remote_dirty.rs` exercising remote-modified-download.
**Step 1.7:** Add `engine_cycle_conflict.rs` (the M4 carry-over) — now writable end-to-end.

**Gate:** `cargo nextest run --workspace`, clippy, fmt. Workspace test count grows by 2.

**Commit:** `feat(graph/core): promote DeltaPage to populated type; close M4 carry-overs`

---

## Task 2: Daemon crate skeleton

**Files:**
- Create: `crates/onesync-daemon/Cargo.toml` (binary crate; deps: every existing crate + `tokio` multi-thread + `tracing` + `signal-hook-tokio` + `fs2` + workspace deps).
- Create: `crates/onesync-daemon/src/main.rs` with minimal `#[tokio::main]` stub.
- Create: module stubs for `startup`, `paths`, `lock`, `wiring`, `logging`, `shutdown`, `ipc/mod.rs`, `methods/mod.rs`, `error.rs`.

Workspace `[workspace.dependencies]` adds:
```toml
tracing            = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
signal-hook-tokio  = { version = "0.3", features = ["futures-v0_3"] }
nix                = { version = "0.29", features = ["signal"] }
hostname           = "0.4"
```

**Gate + commit:** `feat(daemon): onesync-daemon crate skeleton`

---

## Task 3: `paths.rs` — path resolution

Per [`spec/08-installation-and-lifecycle.md`](../spec/08-installation-and-lifecycle.md) §Files and paths. Resolution order: CLI flag → env var → macOS default.

```rust
pub struct Paths {
    pub state_dir: PathBuf,    // ~/Library/Application Support/onesync/
    pub runtime_dir: PathBuf,  // ${TMPDIR}onesync/
    pub log_dir: PathBuf,      // ~/Library/Logs/onesync/
    pub socket: PathBuf,       // <runtime_dir>/onesync.sock
    pub db: PathBuf,           // <state_dir>/onesync.sqlite
    pub pid_file: PathBuf,     // <runtime_dir>/onesyncd.pid
    pub lock_file: PathBuf,    // <state_dir>/onesync.lock
    pub jsonl_log: PathBuf,    // <log_dir>/onesyncd.jsonl
}

impl Paths {
    pub fn resolve(flags: &Flags) -> Result<Self, std::io::Error>;
    pub fn ensure_dirs(&self) -> Result<(), std::io::Error>;
}
```

`ensure_dirs` creates each directory with mode `0700`. Use `std::os::unix::fs::PermissionsExt`.

**Tests:** unit tests with `tempfile` overriding paths; verify directory perms after `ensure_dirs`.

**Commit:** `feat(daemon): path resolution and directory creation`

---

## Task 4: `lock.rs` — advisory lock

`fs2::FileExt::try_lock_exclusive` on `<state_dir>/onesync.lock`. If lock is held by another process, return `EngineError::LocalFs(AlreadyRunning(...))` (via the `LocalFsError` enum from M2).

Write the daemon's PID to `<runtime_dir>/onesyncd.pid` after acquiring the lock; remove on shutdown.

**Tests:** spawn a thread that holds the lock; assert second acquire fails.

**Commit:** `feat(daemon): advisory lock with PID file`

---

## Task 5: `wiring.rs` — port assembly

```rust
pub struct Ports {
    pub state: Arc<onesync_state::SqliteStore>,
    pub local: Arc<onesync_fs_local::LocalFsAdapter>,
    pub remote: Arc<onesync_graph::GraphAdapter>,
    pub vault: Arc<onesync_keychain::KeychainTokenVault>,
    pub clock: Arc<onesync_time::SystemClock>,
    pub ids: Arc<onesync_time::UlidGenerator>,
    pub jitter: Arc<onesync_time::SystemJitter>,
    pub audit: Arc<DaemonAuditSink>,           // see Task 11
}

pub async fn build(paths: &Paths) -> Result<Ports, DaemonError>;
```

`DaemonAuditSink` writes through to both the `StateStore::audit_append` and the structured JSONL log file.

**Commit:** `feat(daemon): port wiring at startup`

---

## Task 6: `MAX_RUNTIME_WORKERS` lands in `limits.rs`

Add to `onesync-core::limits`:
```rust
/// Tokio runtime worker count. Computed at startup as `min(num_cpus, 4)`.
/// Exposed here as a maximum (the upper bound; daemon's `runtime_workers()`
/// returns the actual figure).
pub const MAX_RUNTIME_WORKERS: usize = 4;
```

Plus in `daemon/src/main.rs`:
```rust
fn runtime_workers() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(MAX_RUNTIME_WORKERS)
}
```

Wire into the `#[tokio::main]` macro: `#[tokio::main(flavor = "multi_thread", worker_threads = runtime_workers())]` — actually that doesn't work because the macro evaluates `worker_threads` as a const expression. Use the explicit builder form:

```rust
fn main() -> Result<(), DaemonError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(runtime_workers())
        .enable_all()
        .build()?;
    rt.block_on(async_main())
}
```

**Commit:** `feat(daemon): runtime sizing via MAX_RUNTIME_WORKERS`

---

## Task 7: `ipc/framing.rs` — line-delimited JSON-RPC 2.0

Decoder reads `tokio::io::AsyncBufReadExt::read_line` and parses each line as `serde_json::Value`. Encoder writes `serde_json::to_string(&value)?` + `\n`.

Enforce `IPC_FRAME_MAX_BYTES` (1 MiB): if a line exceeds, close the connection with `RpcError { code: -32600, message: "PayloadTooLarge", data: ... }`.

```rust
pub struct Connection {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: BufWriter<tokio::net::unix::OwnedWriteHalf>,
}

impl Connection {
    pub async fn read_request(&mut self) -> Result<JsonRpcRequest, ConnectionError>;
    pub async fn write_response(&mut self, resp: &JsonRpcResponse) -> Result<(), ConnectionError>;
    pub async fn write_notification(&mut self, notif: &JsonRpcNotification) -> Result<(), ConnectionError>;
}
```

JSON-RPC types in `crates/onesync-protocol/src/rpc.rs` (extend the protocol crate):
```rust
pub struct JsonRpcRequest { pub jsonrpc: String, pub id: Option<String>, pub method: String, pub params: serde_json::Value }
pub struct JsonRpcResponse { pub jsonrpc: String, pub id: String, pub result: Option<serde_json::Value>, pub error: Option<RpcError> }
pub struct JsonRpcNotification { pub jsonrpc: String, pub method: String, pub params: serde_json::Value }
```

Tests: round-trip request/response/notification through the framing.

**Commit:** `feat(daemon/protocol): JSON-RPC 2.0 line-delimited framing`

---

## Task 8: `ipc/server.rs` — Unix socket listener

`UnixListener::bind(&paths.socket)?` then accept loop spawning a Tokio task per connection. Each task reads requests via `Connection::read_request` and routes to `dispatch::handle_request`.

Set socket file permissions to `0600` via `std::os::unix::fs::PermissionsExt` immediately after bind.

Unlink any stale socket file before binding (the daemon's advisory lock guarantees no live process is using it).

**Gate:** spin up the server in a test, connect a `UnixStream` client, send `health.ping`, assert response. Workspace test count +1.

**Commit:** `feat(daemon): Unix socket IPC server with per-connection tasks`

---

## Task 9: `ipc/dispatch.rs` — method routing

```rust
pub async fn handle_request(
    ports: &Ports,
    subs: &SubscriptionManager,
    req: JsonRpcRequest,
) -> JsonRpcResponse;
```

`match req.method.as_str()` against the full method list from spec 07. Each arm delegates to the corresponding `methods/<entity>.rs::method_name`. Unknown method → `RpcError { code: -32601 }`. Invalid params → `RpcError { code: -32602 }`.

Notifications (no `id` on request) are still routed but the response is suppressed.

**Commit:** `feat(daemon): JSON-RPC method dispatch table`

---

## Task 10: Subscription multiplexer (`ipc/subscriptions.rs`)

```rust
pub struct SubscriptionManager {
    subs: Mutex<HashMap<SubscriptionId, SubscriptionEntry>>,
}

pub struct SubscriptionEntry {
    pub kind: SubscriptionKind,
    pub connection_id: ConnectionId,
    pub tx: mpsc::Sender<JsonRpcNotification>,
}

pub enum SubscriptionKind { AuditTail { filter: AuditFilter }, PairStateChanged, ConflictDetected }
```

When a connection drops, its subscriptions are reaped after `SUB_GC_INTERVAL_MS` (already pinned). Backpressure: if `tx.try_send` fails (slow consumer), drop the subscription and audit `subscription.dropped`.

**Tests:** open subscription, push event, assert receipt; close connection, verify GC.

**Commit:** `feat(daemon): subscription multiplexer with backpressure`

---

## Task 11: `logging.rs` — structured JSONL + console

`tracing-subscriber::Registry` with two layers:
- Console (human-readable, `RUST_LOG`-controlled).
- JSON Lines file at `<log_dir>/onesyncd.jsonl`, rotated at `LOG_ROTATE_BYTES` (32 MiB), retain `LOG_RETAIN_FILES` (10).

`DaemonAuditSink` (from Task 5) emits each `AuditEvent` to both the `tracing` macros AND the `StateStore::audit_append` async call (via a Tokio mpsc → background drain task).

**Commit:** `feat(daemon): structured logging with JSONL rotation`

---

## Task 12: Method handlers — health + config

`methods/health.rs`:
- `health.ping` → `{ uptime_s, version, schema_version }`.
- `health.diagnostics` → full `Diagnostics` snapshot (from `onesync-protocol::handles`).

`methods/config.rs`:
- `config.get` / `config.set` / `config.reload` — delegate to `StateStore` config queries.

**Tests:** one per method via in-process client.

**Commit:** `feat(daemon/methods): health and config RPC handlers`

---

## Task 13: Method handlers — account + pair

`methods/account.rs`: `login.begin` returns the OAuth URL + a `login_handle` (UUID); `login.await` blocks the handle's one-shot receiver until the loopback listener accepts the redirect. `list` / `get` / `remove` (`cascade_pairs` flag).

`methods/pair.rs`: `add` (validates local/remote, resolves remote item via `RemoteDrive::item_by_path`, persists), `list` / `get` / `pause` / `resume` / `remove`. `force_sync` returns a `SyncRunHandle` and runs `engine::run_cycle` in a spawned task; `status` returns `PairStatusDetail`. `subscribe` opens a `pair.state_changed` subscription.

**Tests:** add account + pair, list, status; force_sync against fakes.

**Commit:** `feat(daemon/methods): account and pair RPC handlers`

---

## Task 14: Method handlers — remaining

`methods/conflict.rs`: `list` / `get` / `resolve` / `subscribe`.
`methods/audit.rs`: `tail` / `search`.
`methods/run.rs`: `list` / `get` (returns `SyncRunDetail`).
`methods/state.rs`: `backup` / `export` / `repair.permissions` / `compact.now`.
`methods/service.rs`: `shutdown { drain: bool }` / `upgrade.prepare` / `upgrade.commit`.

**Tests:** one happy-path per method.

**Commit:** `feat(daemon/methods): conflict/audit/run/state/service RPC handlers`

---

## Task 15: Signal handling + graceful shutdown

`signal-hook-tokio` listens for SIGTERM/SIGINT. Shutdown sequence:
1. Stop accepting new connections.
2. Notify all in-flight cycles to drain (up to `SHUTDOWN_DRAIN_TIMEOUT_S`).
3. Close socket; unlink it; remove PID file; release advisory lock.
4. Exit 0.

`service.shutdown` RPC triggers the same sequence.

Major-version handshake (`ipc/version.rs`): on every request, compare CLI's claimed major against daemon's. Mismatch → `RpcError { kind: "version.major_mismatch" }`.

**Tests:** spawn daemon in tokio-test, send `service.shutdown`, await exit; assert socket file gone.

**Commit:** `feat(daemon): graceful shutdown, signal handling, version handshake`

---

## Task 16: M5 close

Run the full workspace gate. Update roadmap. Workspace test count target: ≥ 290.

**Commit:** `docs(plans): mark M5 complete on the roadmap`

---

## Self-review checklist

- [ ] `DeltaPage` populated; engine cycle tests for remote-modified and conflict exist and pass.
- [ ] `onesyncd` binary builds and runs (`cargo run -p onesync-daemon`).
- [ ] Socket created at `${TMPDIR}onesync/onesync.sock` with `0600` perms.
- [ ] PID file present at `${TMPDIR}onesync/onesyncd.pid`.
- [ ] Two daemons can't run simultaneously (advisory lock).
- [ ] Every method in spec 07's method table has a handler.
- [ ] Major-version mismatch returns `version.major_mismatch` error.
- [ ] Frame > `IPC_FRAME_MAX_BYTES` → `PayloadTooLarge`.
- [ ] `service.shutdown { drain: true }` waits for in-flight cycles up to `SHUTDOWN_DRAIN_TIMEOUT_S`.
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` exits 0.
- [ ] `cargo fmt --all -- --check` exits 0.

## Carry-overs

- LaunchAgent plist generation, `service install` / `uninstall`, and the upgrade flow live in M7.
- The CLI binary lives in M6; M5 ships only the server side.
