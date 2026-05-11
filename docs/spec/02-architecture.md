# 02 — Architecture

**Status:** Draft · **Date:** 2026-05-11 · **Owner:** Stan

This page defines how the onesync codebase is organised: the crate layout, the port/adapter
boundary, the dependency graph, and the rules for where particular kinds of code live. The
shape is hexagonal: a pure-logic core with no I/O, surrounded by adapter crates that implement
typed ports against real systems. The two binaries (`onesyncd`, `onesync`) wire the pieces
together.

---

## Layering

Three concentric layers; the dependency arrow points inward.

```
   ┌─────────────────────────────────────────────────────┐
   │  Binaries:  onesyncd (daemon)  ·  onesync (CLI)     │
   │  Wire core to adapters; host IPC server / client.   │
   └─────────────────────────────────────────────────────┘
                          ▲
                          │ depends on
                          │
   ┌─────────────────────────────────────────────────────┐
   │  Adapters:                                          │
   │   onesync-state    (SQLite via rusqlite)            │
   │   onesync-fs-local (notify, fs2, blake3)            │
   │   onesync-graph    (reqwest, MSAL public client)    │
   │   onesync-keychain (security-framework)             │
   │   onesync-time     (SystemClock, UlidGenerator)     │
   └─────────────────────────────────────────────────────┘
                          ▲
                          │ depends on
                          │
   ┌─────────────────────────────────────────────────────┐
   │  Core:                                              │
   │   onesync-core      (pure logic: engine, policy,    │
   │                     scheduler, port traits)         │
   │   onesync-protocol  (canonical types,               │
   │                     JSON-RPC wire types)            │
   └─────────────────────────────────────────────────────┘
```

Rules:

1. **`onesync-core` has no I/O.** No `std::fs`, no networking, no time, no logging side effects.
   Logging is a typed event emitted through the `AuditSink` port. Time is requested from the
   `Clock` port. IDs are requested from `IdGenerator`. Anything that reads or writes the world
   is an adapter.
2. **`onesync-core` declares port traits.** Adapters implement them. The daemon binary depends
   on both core and adapters and assembles the concrete implementations behind `&dyn Port`.
3. **`onesync-protocol` is the only crate the CLI and the daemon share for IPC.** It owns the
   JSON-RPC request/response types and the canonical entity DTOs. It must compile without
   any platform-specific deps so it can be a thin dependency of both binaries.
4. **No adapter depends on another adapter.** Cross-adapter coordination happens in core.
5. **Errors do not cross layer boundaries unchanged.** Each adapter has its own `Error` enum;
   ports return `Result<T, PortError>` where `PortError` is defined alongside the trait in
   `onesync-core`. Adapters map their internal errors into the port's error.

---

## Crate layout

```
onesync/
├── Cargo.toml                      (workspace root)
├── crates/
│   ├── onesync-protocol/           (no_std-friendly; serde types only)
│   ├── onesync-core/               (engine, ports, policies)
│   ├── onesync-state/              (SQLite adapter, migrations)
│   ├── onesync-fs-local/           (FSEvents, blake3, fs2 locks)
│   ├── onesync-graph/              (Microsoft Graph + MSAL client)
│   ├── onesync-keychain/           (macOS Keychain Services)
│   ├── onesync-time/               (system clock, ULID generator)
│   ├── onesync-daemon/             (binary: onesyncd)
│   └── onesync-cli/                (binary: onesync)
├── docs/
└── tests/
    ├── integration/                (real SQLite, fake graph, fake fs)
    └── e2e/                        (real graph against test tenant; tier=slow)
```

Per the [development guidelines](09-development-guidelines.md), each crate carries a
`#![forbid(unsafe_code)]` attribute at the crate root, except those that wrap macOS frameworks:

| Crate | `unsafe_code` |
|---|---|
| `onesync-protocol` | forbid |
| `onesync-core` | forbid |
| `onesync-state` | forbid |
| `onesync-time` | forbid |
| `onesync-daemon` | forbid |
| `onesync-cli` | forbid |
| `onesync-graph` | forbid |
| `onesync-fs-local` | forbid (notify wraps FSEvents internally) |
| `onesync-keychain` | allowed only via vetted `security-framework` shim |

Where `unsafe` is permitted, every block carries a `// SAFETY:` comment that names the
invariant being upheld; reviewers check those comments rather than allowing free-form `unsafe`.

---

## Ports

The core defines these traits. The list is complete; if a new I/O dependency is needed, it
must arrive as a new port, not as a direct call.

### `StateStore`

```rust
#[async_trait]
pub trait StateStore: Send + Sync {
    async fn account_upsert(&self, account: &Account) -> Result<(), StateError>;
    async fn account_get(&self, id: &AccountId) -> Result<Option<Account>, StateError>;
    async fn pair_upsert(&self, pair: &Pair) -> Result<(), StateError>;
    async fn pair_get(&self, id: &PairId) -> Result<Option<Pair>, StateError>;
    async fn pairs_active(&self) -> Result<Vec<Pair>, StateError>;
    async fn file_entry_upsert(&self, entry: &FileEntry) -> Result<(), StateError>;
    async fn file_entry_get(&self, pair: &PairId, path: &RelPath)
        -> Result<Option<FileEntry>, StateError>;
    async fn file_entries_dirty(&self, pair: &PairId, limit: usize)
        -> Result<Vec<FileEntry>, StateError>;
    async fn run_record(&self, run: &SyncRun) -> Result<(), StateError>;
    async fn op_insert(&self, op: &FileOp) -> Result<(), StateError>;
    async fn op_update_status(&self, id: &FileOpId, status: FileOpStatus)
        -> Result<(), StateError>;
    async fn conflict_insert(&self, c: &Conflict) -> Result<(), StateError>;
    async fn conflicts_unresolved(&self, pair: &PairId) -> Result<Vec<Conflict>, StateError>;
    async fn audit_append(&self, evt: &AuditEvent) -> Result<(), StateError>;
}
```

### `LocalFs`

```rust
#[async_trait]
pub trait LocalFs: Send + Sync {
    async fn scan(&self, root: &Path) -> Result<LocalScanStream, LocalFsError>;
    async fn read(&self, path: &Path) -> Result<LocalReadStream, LocalFsError>;
    async fn write_atomic(&self, path: &Path, stream: LocalWriteStream)
        -> Result<FileSide, LocalFsError>;
    async fn rename(&self, from: &Path, to: &Path) -> Result<(), LocalFsError>;
    async fn delete(&self, path: &Path) -> Result<(), LocalFsError>;
    async fn mkdir_p(&self, path: &Path) -> Result<(), LocalFsError>;
    async fn watch(&self, root: &Path) -> Result<LocalEventStream, LocalFsError>;
    async fn hash(&self, path: &Path) -> Result<ContentHash, LocalFsError>;
}
```

### `RemoteDrive`

```rust
#[async_trait]
pub trait RemoteDrive: Send + Sync {
    async fn account_profile(&self, token: &AccessToken) -> Result<AccountProfile, GraphError>;
    async fn item_by_path(&self, drive: &DriveId, path: &str)
        -> Result<Option<RemoteItem>, GraphError>;
    async fn delta(&self, drive: &DriveId, cursor: Option<&DeltaCursor>)
        -> Result<DeltaPage, GraphError>;
    async fn download(&self, item: &RemoteItemId)
        -> Result<RemoteReadStream, GraphError>;
    async fn upload_small(&self, parent: &RemoteItemId, name: &str, bytes: &[u8])
        -> Result<RemoteItem, GraphError>;
    async fn upload_session(&self, parent: &RemoteItemId, name: &str, size: u64)
        -> Result<UploadSession, GraphError>;
    async fn rename(&self, item: &RemoteItemId, new_name: &str)
        -> Result<RemoteItem, GraphError>;
    async fn delete(&self, item: &RemoteItemId) -> Result<(), GraphError>;
    async fn mkdir(&self, parent: &RemoteItemId, name: &str)
        -> Result<RemoteItem, GraphError>;
}
```

### `TokenVault`

```rust
#[async_trait]
pub trait TokenVault: Send + Sync {
    async fn store_refresh(&self, account: &AccountId, token: &RefreshToken)
        -> Result<KeychainRef, VaultError>;
    async fn load_refresh(&self, account: &AccountId)
        -> Result<RefreshToken, VaultError>;
    async fn delete(&self, account: &AccountId) -> Result<(), VaultError>;
}
```

### `Clock`

```rust
pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}
```

### `IdGenerator`

```rust
pub trait IdGenerator: Send + Sync {
    fn new_id<T: IdPrefix>(&self) -> Id<T>;
}
```

### `AuditSink`

```rust
pub trait AuditSink: Send + Sync {
    fn emit(&self, event: AuditEvent);
}
```

Each port has both a production adapter and a fake under `crates/<adapter>/src/fakes.rs` for
tests. Fakes implement the trait against in-memory state; the core's integration tests run
against fakes only.

---

## Dependency graph

```
                         ┌──────────────────┐
                         │ onesync-protocol │
                         └─────────┬────────┘
                                   │
              ┌────────────────────┼────────────────────┐
              │                    │                    │
              ▼                    ▼                    ▼
       onesync-core         onesync-daemon         onesync-cli
              │                    │
              │              ┌─────┴───────────────────┐
              │              │       │      │       │  │
              ▼              ▼       ▼      ▼       ▼  ▼
       (port traits)    onesync-  onesync-  onesync- onesync-
                        state     fs-local  graph    keychain
                                                     onesync-time
```

Cycle prevention is enforced by Cargo. The CLI never depends on adapter crates. The core
never depends on adapter crates. The daemon is the only crate allowed to combine adapters
with core.

---

## Concurrency model

- The daemon runs on Tokio's multi-threaded runtime with a bounded worker pool sized to
  `MAX_RUNTIME_WORKERS`.
- Each `Pair` has its own `mpsc` event channel and a single owning task that drives the sync
  loop for that pair. Pairs do not share mutable state; cross-pair coordination goes through
  the shared `StateStore` only.
- Adapter calls are async. The graph adapter holds a per-`Account` rate-limit token bucket
  bounded by `GRAPH_RPS_PER_ACCOUNT`.
- File I/O uses `tokio::task::spawn_blocking` for hashing and reads larger than `READ_INLINE_MAX`
  to keep the runtime responsive.
- The IPC server runs on its own task and dispatches each RPC to a method handler;
  long-running calls (`pair_force_sync`) return a handle and stream progress via subscriptions.

No mutex protects domain state across awaits. State changes go through `StateStore` and are
made durable before any observable side-effect (especially before a remote write).

---

## Configuration

There is no separate config file. Configuration lives in the state database as a singleton
`InstanceConfig` row, mutated through the CLI. The daemon reads it at startup and on
explicit `config_reload` RPCs. Limits are compile-time `const`s (see
[`09-development-guidelines.md`](09-development-guidelines.md)) and are not user-tunable.

The instance config carries operator-set fields only:

- log level (`info`/`debug`/`trace`)
- whether to use system notifications for non-fatal warnings
- network egress preferences (allow metered? off by default on macOS-detected metered networks)
- minimum free disk-space margin to keep before pausing downloads

Compile-time limits and operator-set values are deliberately disjoint; a value never appears
in both places.

---

## Build profile

- `profile.release` enables `lto = "thin"`, `codegen-units = 1`, `panic = "abort"`.
- `profile.dev` enables `overflow-checks = true` and `debug = "limited"`.
- Workspace `clippy.toml` enables pedantic + nursery; specific opt-outs require a `// LINT:`
  comment with the reason.

---

## Assumptions and open questions

**Assumptions**

- Tokio is the only async runtime in the codebase. No `async-std` or `smol` deps.
- `rusqlite` plus the `bundled` feature ships SQLite with the binary; no system SQLite is
  expected.
- The `notify` crate's `FsEventWatcher` backend is the FSEvents-backed implementation on macOS
  and is sufficient. We do not need to bind CoreServices directly.

**Decisions**

- *Hexagonal with one crate per adapter.* **Each adapter is its own crate, even small ones.**
  Forces explicit dependencies, isolates compile times, and makes "which crate touches the
  network" trivial to answer.
- *Async trait objects via `async-trait`.* **`#[async_trait]` ports, `&dyn Port` injection.**
  The runtime cost is acceptable; we avoid generics noise in core types. Re-evaluate when
  native async-fn-in-trait stabilises with object safety.
- *Errors typed per crate.* **One `ThisCrateError` enum per adapter, mapped to a port-level
  error in core.** Prevents leaky abstractions where the engine inspects a `reqwest::Error`.

**Open questions**

- *Whether `onesync-protocol` should also serve as the canonical types crate for tests.* It
  currently holds both wire types and domain DTOs; that may need to split if the protocol
  evolves faster than the domain.
- *Tokio runtime sizing.* `MAX_RUNTIME_WORKERS` defaults to `min(num_cpus, 4)`. We do not yet
  have evidence of contention; revisit after first production runs.
