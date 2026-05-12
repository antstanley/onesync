# onesync M2 — State + Local FS Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the two adapter crates that ground the engine in real I/O — `onesync-state` (SQLite via `rusqlite` + `refinery` migrations, implementing the `StateStore` port) and `onesync-fs-local` (FSEvents-driven watcher, BLAKE3 hashing, atomic writes, implementing the `LocalFs` port). Plus a workspace CI scaffold so the gate runs on every PR.

**Architecture:** Both crates implement port traits from `onesync-core` against real backends and ship in-memory fakes for engine tests. No engine logic lives here — the crates are pure adapter implementations. State store opens a single per-user SQLite file in WAL mode; local-fs uses `notify`'s `RecommendedWatcher` (FSEvents on macOS), `tempfile` for atomic-rename writes, and `blake3` for content hashes.

**Tech Stack:** Rust 1.95.0; new deps `rusqlite` (with `bundled` feature so SQLite ships in the binary), `refinery`, `r2d2` + `r2d2_sqlite`, `notify`, `blake3`, `tempfile`, `fs2`, `tokio` (already pinned). Test runner remains `cargo-nextest`. macOS-only test runners (`darwin-aarch64` and `darwin-x86_64`).

VCS: `jj` colocated. Per-task commits on `main`. Co-Authored-By trailer: `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` verbatim on every commit.

---

## Pre-flight

- M1 is complete and pushed to `origin/main` at `55f695ff` (verified). Workspace has three crates (`onesync-protocol`, `onesync-core`, `onesync-time`) plus 35 passing tests.
- Spec source of truth: [`docs/spec/05-local-adapter.md`](../spec/05-local-adapter.md) and [`docs/spec/06-state-store.md`](../spec/06-state-store.md). Read these before starting.
- Workspace `Cargo.toml` (`[workspace.dependencies]`) already has `tokio` and `thiserror`. Tasks add `rusqlite`, `refinery`, `r2d2`, `r2d2_sqlite`, `notify`, `blake3`, `tempfile`, `fs2` to the workspace table.
- All new crates carry `#![forbid(unsafe_code)]` at the crate root. The single exception is `onesync-fs-local`, where the spec permits `unsafe` only inside vetted FFI shims; `notify` wraps FSEvents internally so our own code stays `unsafe`-free. Confirm with a workspace-wide `git grep "unsafe " crates/` post-M2.
- macOS-only crates need a `cfg(target_os = "macos")` gate at the crate root for any FSEvents-specific code paths. `notify` itself is cross-platform, so the gate only goes on macOS-specific helpers (DiskArbitration polling, mount/unmount detection, etc.).
- Per-task commits ship a single conventional-commit subject and the Opus 4.7 trailer. Use `jj describe -m "..."` then `jj new`. Do NOT advance the `main` bookmark mid-milestone — Task 22 does that once at M2 close.

---

## File map (what M2 creates)

```
onesync/
├── .github/workflows/ci.yml                      # Task 21
├── crates/
│   ├── onesync-state/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── connection.rs                     # pool, open, PRAGMAs
│   │       ├── migrations/
│   │       │   ├── mod.rs                        # refinery embed
│   │       │   ├── V001__initial_schema.sql      # all 8 tables + indexes
│   │       │   └── ...
│   │       ├── queries/
│   │       │   ├── mod.rs
│   │       │   ├── accounts.rs
│   │       │   ├── pairs.rs
│   │       │   ├── file_entries.rs
│   │       │   ├── file_ops.rs
│   │       │   ├── conflicts.rs
│   │       │   ├── sync_runs.rs
│   │       │   ├── audit.rs
│   │       │   └── config.rs
│   │       ├── store.rs                          # StateStore trait impl
│   │       ├── retention.rs                      # compaction job
│   │       ├── error.rs                          # StateStoreError -> StateError
│   │       └── fakes.rs                          # in-memory fake StateStore
│   └── onesync-fs-local/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── path.rs                           # canonicalisation helpers
│           ├── hash.rs                           # BLAKE3 streaming + race detection
│           ├── write.rs                          # atomic temp+rename+fsync
│           ├── ops.rs                            # rename, delete, mkdir_p
│           ├── scan.rs                           # bounded BFS scan
│           ├── watcher.rs                        # FSEvents via notify, overflow
│           ├── volumes.rs                        # mount/unmount detection
│           ├── adapter.rs                        # LocalFs trait impl
│           ├── error.rs                          # LocalFsError mapping
│           └── fakes.rs                          # in-memory fake LocalFs
└── xtask/                                        # Task 10
    ├── Cargo.toml
    └── src/main.rs
```

22 tasks: Phase A (`onesync-state`) is Tasks 1–10, Phase B (`onesync-fs-local`) is Tasks 11–19, integration + CI + close is Tasks 20–22.

---

# Phase A — `onesync-state`

## Task 1: Crate skeleton + workspace deps

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)
- Create: `crates/onesync-state/Cargo.toml`
- Create: `crates/onesync-state/src/lib.rs`

- [ ] **Step 1.1: Extend workspace `[workspace.dependencies]`**

Append to the existing table (preserve all existing entries):

```toml
rusqlite      = { version = "0.32", features = ["bundled", "chrono", "serde_json"] }
r2d2          = "0.8"
r2d2_sqlite   = "0.25"
refinery      = { version = "0.8", features = ["rusqlite"] }
tempfile      = "3"
fs2           = "0.4"
notify        = "6"
blake3        = "1"
```

(`rusqlite` versions ≥ 0.32 + `r2d2_sqlite` 0.25 are compatible; check at install time and bump if necessary, but stay on a stable major.)

- [ ] **Step 1.2: Create `crates/onesync-state/Cargo.toml`**

```toml
[package]
name = "onesync-state"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
onesync-core     = { path = "../onesync-core" }
onesync-protocol = { path = "../onesync-protocol" }
async-trait      = { workspace = true }
chrono           = { workspace = true }
rusqlite         = { workspace = true }
r2d2             = { workspace = true }
r2d2_sqlite      = { workspace = true }
refinery         = { workspace = true }
serde            = { workspace = true }
serde_json       = { workspace = true }
thiserror        = { workspace = true }
tokio            = { workspace = true }
ulid             = { workspace = true }

[dev-dependencies]
tempfile         = { workspace = true }
proptest         = { workspace = true }
```

- [ ] **Step 1.3: Create `crates/onesync-state/src/lib.rs`**

```rust
//! SQLite adapter implementing the `StateStore` port.
//!
//! See [`docs/spec/06-state-store.md`](../../../../docs/spec/06-state-store.md).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod connection;
pub mod error;
pub mod fakes;
pub mod migrations;
pub mod queries;
pub mod retention;
pub mod store;

pub use connection::{open, ConnectionPool};
pub use error::StateStoreError;
pub use store::SqliteStore;
```

- [ ] **Step 1.4: Stub the modules**

Create empty files for each declared module so `cargo check` parses:

```rust
// crates/onesync-state/src/connection.rs
//! SQLite connection pool and PRAGMAs.

// crates/onesync-state/src/error.rs
//! Internal error type, mapped to `StateError` at the port boundary.

// crates/onesync-state/src/fakes.rs
//! In-memory `StateStore` implementation for tests.

// crates/onesync-state/src/migrations/mod.rs
//! Embedded migrations driven by `refinery`.

// crates/onesync-state/src/queries/mod.rs
//! Per-table query helpers.

// crates/onesync-state/src/retention.rs
//! Compaction and pruning job.

// crates/onesync-state/src/store.rs
//! `SqliteStore` — concrete `StateStore` adapter.
```

Add minimal placeholders inside `connection.rs` so the `pub use` lines in `lib.rs` resolve:

```rust
//! SQLite connection pool and PRAGMAs.

use std::path::Path;

/// Placeholder for the connection pool type; Task 3 implements it.
pub struct ConnectionPool;

/// Placeholder for the open-and-migrate entry point; Task 3 implements it.
///
/// # Errors
/// Returns an error when the database cannot be opened or migrated.
pub fn open(_path: &Path) -> Result<ConnectionPool, crate::error::StateStoreError> {
    unimplemented!("Task 3 implements this")
}
```

And in `error.rs`:

```rust
//! Internal error type, mapped to `StateError` at the port boundary.

/// Errors raised by `onesync-state` internals.
#[derive(Debug, thiserror::Error)]
pub enum StateStoreError {
    /// Underlying SQLite / pool failure.
    #[error("sqlite: {0}")]
    Sqlite(String),
    /// Migration failure.
    #[error("migration: {0}")]
    Migration(String),
    /// Schema mismatch detected at open time.
    #[error("schema: {0}")]
    Schema(String),
}
```

And in `store.rs`:

```rust
//! `SqliteStore` — concrete `StateStore` adapter.

/// Placeholder until Task 8 implements the adapter.
pub struct SqliteStore;
```

Workspace clippy may complain about `unimplemented!()` — that lint is workspace-`deny`. For Task 1 only, add a targeted `#[allow(clippy::unimplemented)]` on `open` with a `// LINT:` comment naming the task that fills it in.

- [ ] **Step 1.5: Run gates**

```
cargo check -p onesync-state
cargo clippy -p onesync-state --all-targets -- -D warnings
cargo fmt --all -- --check
cargo nextest run --workspace
```

Expected: 35 tests still pass; clippy + fmt clean.

- [ ] **Step 1.6: Commit**

```
jj describe -m "feat(state): onesync-state crate skeleton with module stubs

Adds the workspace-level dependency pins for the SQLite stack (rusqlite,
r2d2, r2d2_sqlite, refinery, tempfile, fs2, notify, blake3) and the empty
shell of the onesync-state crate: lib.rs, error.rs, connection.rs, store.rs,
plus stubs for migrations, queries, retention, and fakes. Subsequent M2
tasks fill these out.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
jj new
```

---

## Task 2: Initial schema migration (V001)

**Files:**
- Create: `crates/onesync-state/src/migrations/V001__initial_schema.sql`
- Modify: `crates/onesync-state/src/migrations/mod.rs`
- Modify: `crates/onesync-state/Cargo.toml` to include `refinery` + `refinery_core` build setup

This task lands the entire schema in one migration. Splitting into per-table migrations gives no benefit at this stage (nothing exists yet in production), and `refinery` requires monotonic version numbers anyway. Subsequent schema changes ship as V002, V003, … one per release that needs them.

- [ ] **Step 2.1: Write `V001__initial_schema.sql`**

The SQL below is the literal content of [`docs/spec/06-state-store.md`](../spec/06-state-store.md) §Tables, in dependency order (parents before children). Foreign-key constraints reference forward declarations only where SQLite's deferred-FK semantics allow.

```sql
-- onesync-state V001 — initial schema
--
-- Mirrors docs/spec/06-state-store.md. Subsequent migrations must be
-- additive; never rewrite this file.

CREATE TABLE accounts (
    id              TEXT PRIMARY KEY,
    kind            TEXT NOT NULL CHECK (kind IN ('personal','business')),
    upn             TEXT NOT NULL,
    tenant_id       TEXT NOT NULL,
    drive_id        TEXT NOT NULL,
    display_name    TEXT NOT NULL,
    keychain_ref    TEXT NOT NULL,
    scopes_json     TEXT NOT NULL,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
);
CREATE UNIQUE INDEX accounts_upn_uq ON accounts(upn);

CREATE TABLE pairs (
    id                 TEXT PRIMARY KEY,
    account_id         TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    local_path         TEXT NOT NULL,
    remote_item_id     TEXT NOT NULL,
    remote_path        TEXT NOT NULL,
    display_name       TEXT NOT NULL,
    status             TEXT NOT NULL CHECK
                          (status IN ('initializing','active','paused','errored','removed')),
    paused             INTEGER NOT NULL DEFAULT 0,
    delta_token        TEXT,
    errored_reason     TEXT,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL,
    last_sync_at       TEXT,
    conflict_count     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX pairs_account_idx ON pairs(account_id);
CREATE UNIQUE INDEX pairs_local_path_uq ON pairs(local_path) WHERE status <> 'removed';
CREATE UNIQUE INDEX pairs_remote_uq
    ON pairs(account_id, remote_item_id) WHERE status <> 'removed';

CREATE TABLE sync_runs (
    id                 TEXT PRIMARY KEY,
    pair_id            TEXT NOT NULL REFERENCES pairs(id) ON DELETE CASCADE,
    trigger            TEXT NOT NULL,
    started_at         TEXT NOT NULL,
    finished_at        TEXT,
    local_ops          INTEGER NOT NULL DEFAULT 0,
    remote_ops         INTEGER NOT NULL DEFAULT 0,
    bytes_uploaded     INTEGER NOT NULL DEFAULT 0,
    bytes_downloaded   INTEGER NOT NULL DEFAULT 0,
    outcome            TEXT,
    outcome_detail     TEXT
);
CREATE INDEX sync_runs_pair_started_idx ON sync_runs(pair_id, started_at DESC);

CREATE TABLE file_ops (
    id                 TEXT PRIMARY KEY,
    run_id             TEXT NOT NULL REFERENCES sync_runs(id) ON DELETE CASCADE,
    pair_id            TEXT NOT NULL REFERENCES pairs(id) ON DELETE CASCADE,
    relative_path      TEXT NOT NULL,
    kind               TEXT NOT NULL,
    status             TEXT NOT NULL,
    attempts           INTEGER NOT NULL DEFAULT 0,
    last_error_json    TEXT,
    metadata_json      TEXT,
    enqueued_at        TEXT NOT NULL,
    started_at         TEXT,
    finished_at        TEXT
);
CREATE INDEX file_ops_pair_status_idx ON file_ops(pair_id, status);
CREATE INDEX file_ops_run_idx ON file_ops(run_id);

CREATE TABLE file_entries (
    pair_id            TEXT NOT NULL REFERENCES pairs(id) ON DELETE CASCADE,
    relative_path      TEXT NOT NULL,
    kind               TEXT NOT NULL CHECK (kind IN ('file','directory')),
    sync_state         TEXT NOT NULL CHECK
                          (sync_state IN ('clean','dirty','pending_upload',
                                          'pending_download','pending_conflict','in_flight')),
    local_json         TEXT,
    remote_json        TEXT,
    synced_json        TEXT,
    pending_op_id      TEXT REFERENCES file_ops(id) ON DELETE SET NULL,
    updated_at         TEXT NOT NULL,
    PRIMARY KEY (pair_id, relative_path)
);
CREATE INDEX file_entries_dirty_idx
    ON file_entries(pair_id, sync_state)
    WHERE sync_state <> 'clean';

CREATE TABLE conflicts (
    id                     TEXT PRIMARY KEY,
    pair_id                TEXT NOT NULL REFERENCES pairs(id) ON DELETE CASCADE,
    relative_path          TEXT NOT NULL,
    winner                 TEXT NOT NULL CHECK (winner IN ('local','remote')),
    loser_relative_path    TEXT NOT NULL,
    local_side_json        TEXT NOT NULL,
    remote_side_json       TEXT NOT NULL,
    detected_at            TEXT NOT NULL,
    resolved_at            TEXT,
    resolution             TEXT,
    note                   TEXT
);
CREATE INDEX conflicts_pair_unresolved_idx
    ON conflicts(pair_id)
    WHERE resolved_at IS NULL;

CREATE TABLE audit_events (
    id            TEXT PRIMARY KEY,
    ts            TEXT NOT NULL,
    level         TEXT NOT NULL CHECK (level IN ('info','warn','error')),
    kind          TEXT NOT NULL,
    pair_id       TEXT REFERENCES pairs(id) ON DELETE SET NULL,
    payload_json  TEXT NOT NULL
);
CREATE INDEX audit_events_ts_idx ON audit_events(ts DESC);

CREATE TABLE instance_config (
    id            INTEGER PRIMARY KEY CHECK (id = 1),
    log_level     TEXT NOT NULL DEFAULT 'info',
    notify        INTEGER NOT NULL DEFAULT 1,
    allow_metered INTEGER NOT NULL DEFAULT 0,
    min_free_gib  INTEGER NOT NULL DEFAULT 2,
    updated_at    TEXT NOT NULL
);
```

- [ ] **Step 2.2: Wire `refinery` in `migrations/mod.rs`**

```rust
//! Embedded migrations driven by `refinery`.

use refinery::embed_migrations;

embed_migrations!("src/migrations");

/// Apply any pending migrations to the given connection.
///
/// # Errors
/// Returns the underlying refinery error when a migration fails to apply.
pub fn run(conn: &mut rusqlite::Connection) -> Result<(), crate::error::StateStoreError> {
    runner()
        .run(conn)
        .map_err(|e| crate::error::StateStoreError::Migration(e.to_string()))?;
    Ok(())
}
```

- [ ] **Step 2.3: Test — schema applies cleanly on a fresh `:memory:` db**

Create `crates/onesync-state/src/migrations/tests.rs` (or a `#[cfg(test)] mod tests` block at the end of `mod.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_apply_to_fresh_memory_db() {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open memory db");
        run(&mut conn).expect("apply migrations");

        // sanity-check: every expected table exists
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
            .expect("prepare")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .map(|r| r.expect("row"))
            .collect();

        for expected in [
            "accounts",
            "audit_events",
            "conflicts",
            "file_entries",
            "file_ops",
            "instance_config",
            "pairs",
            "refinery_schema_history",
            "sync_runs",
        ] {
            assert!(tables.contains(&expected.to_string()), "missing table {expected}");
        }
    }

    #[test]
    fn migrations_are_idempotent_on_second_run() {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open memory db");
        run(&mut conn).expect("first run");
        run(&mut conn).expect("second run — must be a no-op");
    }
}
```

- [ ] **Step 2.4: Gate**

`cargo nextest run -p onesync-state` shows the two migration tests passing. Workspace count: 37.

- [ ] **Step 2.5: Commit**

`feat(state): V001 initial schema migration with full table set`

---

## Task 3: Connection pool + PRAGMAs + `open`

**Files:**
- Modify: `crates/onesync-state/src/connection.rs`
- Modify: `crates/onesync-core/src/limits.rs` — no change; reuse `STATE_POOL_SIZE`

- [ ] **Step 3.1: Implement `ConnectionPool` and `open`**

```rust
//! SQLite connection pool and PRAGMAs.

use std::path::Path;
use std::path::PathBuf;

use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;

use onesync_core::limits::STATE_POOL_SIZE;

use crate::error::StateStoreError;

/// Pool of SQLite connections plus the database file path.
#[derive(Clone)]
pub struct ConnectionPool {
    inner: Pool<SqliteConnectionManager>,
    path: PathBuf,
}

impl ConnectionPool {
    /// Borrow a connection from the pool.
    ///
    /// # Errors
    /// Returns an error if no connection is available within the pool's timeout.
    pub fn get(&self) -> Result<PooledConnection<SqliteConnectionManager>, StateStoreError> {
        self.inner
            .get()
            .map_err(|e| StateStoreError::Sqlite(format!("pool: {e}")))
    }

    /// The on-disk path of the database.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Open (or create) the database at `path`, apply all pending migrations,
/// set the standard PRAGMAs, and return a connection pool.
///
/// # Errors
/// Returns `StateStoreError::Sqlite` for I/O / pool failures and
/// `StateStoreError::Migration` for migration failures.
pub fn open(path: &Path) -> Result<ConnectionPool, StateStoreError> {
    let manager = SqliteConnectionManager::file(path).with_init(|conn| {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "busy_timeout", 5_000)?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        Ok(())
    });

    let pool = Pool::builder()
        .max_size(u32::try_from(STATE_POOL_SIZE).unwrap_or(4))
        .build(manager)
        .map_err(|e| StateStoreError::Sqlite(format!("build pool: {e}")))?;

    // Apply migrations on a single connection before publishing the pool.
    let mut conn: Connection = pool
        .get()
        .map_err(|e| StateStoreError::Sqlite(format!("migration conn: {e}")))?
        .into();
    crate::migrations::run(&mut conn)?;

    Ok(ConnectionPool { inner: pool, path: path.to_owned() })
}
```

Note: `r2d2_sqlite::SqliteConnectionManager::file(path).with_init(|conn| { … })` is the documented hook for setting PRAGMAs on each new connection. If the API in 0.25 differs slightly, adjust to the equivalent method.

- [ ] **Step 3.2: Test — open creates the file and PRAGMAs are set**

Add to `connection.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn open_creates_file_and_sets_wal_mode() {
        let tmp = TempDir::new().expect("tmpdir");
        let db_path = tmp.path().join("test.sqlite");
        let pool = open(&db_path).expect("open");
        assert!(db_path.exists(), "db file should be created");

        let conn = pool.get().expect("get conn");
        let mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("pragma");
        assert_eq!(mode, "wal");
    }

    #[test]
    fn open_is_idempotent_across_reopens() {
        let tmp = TempDir::new().expect("tmpdir");
        let db_path = tmp.path().join("test.sqlite");

        let pool1 = open(&db_path).expect("first open");
        drop(pool1);
        let _pool2 = open(&db_path).expect("second open");
    }

    #[test]
    fn open_rejects_a_directory_path() {
        let tmp = TempDir::new().expect("tmpdir");
        assert!(open(tmp.path()).is_err());
    }
}
```

- [ ] **Step 3.3: Gate + commit**

`cargo nextest run -p onesync-state` shows 5 passing tests (2 migrations + 3 connection). Workspace: 40.

Commit: `feat(state): connection pool with PRAGMAs and migration application`

---

## Task 4: Accounts + pairs queries

**Files:**
- Create: `crates/onesync-state/src/queries/mod.rs` (replaces stub)
- Create: `crates/onesync-state/src/queries/accounts.rs`
- Create: `crates/onesync-state/src/queries/pairs.rs`

Each `queries/<entity>.rs` exposes free functions that take a `&Connection` and operate on a single connection (the trait impl in Task 8 borrows from the pool and dispatches).

- [ ] **Step 4.1: `queries/mod.rs`**

```rust
//! Per-table query helpers. Each module is `pub(crate)` — the public surface is
//! `SqliteStore` in `crate::store`.

pub mod accounts;
pub mod pairs;
```

(Subsequent tasks add `pub mod file_entries; pub mod file_ops; pub mod conflicts; pub mod sync_runs; pub mod audit; pub mod config;`.)

- [ ] **Step 4.2: `queries/accounts.rs`**

```rust
//! Account queries.

use rusqlite::{params, Connection, OptionalExtension};

use onesync_protocol::{
    account::Account,
    enums::AccountKind,
    id::AccountId,
    primitives::{DriveId, KeychainRef, Timestamp},
};

use crate::error::StateStoreError;

/// Insert or replace an account row.
pub fn upsert(conn: &Connection, account: &Account) -> Result<(), StateStoreError> {
    let scopes_json = serde_json::to_string(&account.scopes)
        .map_err(|e| StateStoreError::Sqlite(format!("encode scopes: {e}")))?;
    conn.execute(
        "INSERT INTO accounts \
            (id, kind, upn, tenant_id, drive_id, display_name, keychain_ref, scopes_json, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
            kind = excluded.kind, \
            upn = excluded.upn, \
            tenant_id = excluded.tenant_id, \
            drive_id = excluded.drive_id, \
            display_name = excluded.display_name, \
            keychain_ref = excluded.keychain_ref, \
            scopes_json = excluded.scopes_json, \
            updated_at = excluded.updated_at",
        params![
            account.id.to_string(),
            kind_to_str(account.kind),
            account.upn,
            account.tenant_id,
            account.drive_id.as_str(),
            account.display_name,
            account.keychain_ref.as_str(),
            scopes_json,
            account.created_at.into_inner().to_rfc3339(),
            account.updated_at.into_inner().to_rfc3339(),
        ],
    )
    .map(|_| ())
    .map_err(|e| StateStoreError::Sqlite(e.to_string()))
}

/// Fetch an account by id.
pub fn get(conn: &Connection, id: &AccountId) -> Result<Option<Account>, StateStoreError> {
    let row = conn
        .query_row(
            "SELECT id, kind, upn, tenant_id, drive_id, display_name, keychain_ref, scopes_json, created_at, updated_at \
             FROM accounts WHERE id = ?",
            params![id.to_string()],
            row_to_account,
        )
        .optional()
        .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;
    Ok(row)
}

fn row_to_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    let id_str: String = row.get(0)?;
    let kind_str: String = row.get(1)?;
    let scopes_json: String = row.get(7)?;
    let created_at: String = row.get(8)?;
    let updated_at: String = row.get(9)?;

    Ok(Account {
        id: id_str.parse().map_err(|e: onesync_protocol::id::IdParseError| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(e))
        })?,
        kind: kind_from_str(&kind_str)?,
        upn: row.get(2)?,
        tenant_id: row.get(3)?,
        drive_id: DriveId::new(row.get::<_, String>(4)?),
        display_name: row.get(5)?,
        keychain_ref: KeychainRef::new(row.get::<_, String>(6)?),
        scopes: serde_json::from_str(&scopes_json).map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(e))
        })?,
        created_at: parse_timestamp(&created_at)?,
        updated_at: parse_timestamp(&updated_at)?,
    })
}

fn kind_to_str(k: AccountKind) -> &'static str {
    match k {
        AccountKind::Personal => "personal",
        AccountKind::Business => "business",
    }
}

fn kind_from_str(s: &str) -> rusqlite::Result<AccountKind> {
    match s {
        "personal" => Ok(AccountKind::Personal),
        "business" => Ok(AccountKind::Business),
        other => Err(rusqlite::Error::InvalidColumnType(
            1,
            format!("unknown account kind: {other}"),
            rusqlite::types::Type::Text,
        )),
    }
}

fn parse_timestamp(s: &str) -> rusqlite::Result<Timestamp> {
    let dt = chrono::DateTime::parse_from_rfc3339(s)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    Ok(Timestamp::from_datetime(dt.with_timezone(&chrono::Utc)))
}
```

- [ ] **Step 4.3: `queries/pairs.rs`**

Pattern is identical to accounts. Map `Pair` → row, row → `Pair`. Use the same `parse_timestamp` helper (either duplicate it or hoist to `queries/mod.rs`). For columns `delta_token`, `errored_reason`, `last_sync_at`: nullable. For `status` and `paused`: convert via small helpers. For `local_path`: `AbsPath` is a string; serialise via `.as_str()`, deserialise via `.parse()`. Expose:

- `upsert(conn: &Connection, pair: &Pair) -> Result<(), StateStoreError>`
- `get(conn: &Connection, id: &PairId) -> Result<Option<Pair>, StateStoreError>`
- `active(conn: &Connection) -> Result<Vec<Pair>, StateStoreError>` — `SELECT … WHERE status <> 'removed'`

(Write the full implementation — no stubs. Use accounts.rs as the template.)

- [ ] **Step 4.4: Tests**

Append to each module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open;
    use chrono::{TimeZone, Utc};
    use onesync_protocol::id::{Id, AccountTag};
    use tempfile::TempDir;
    use ulid::Ulid;

    fn fresh_db() -> (TempDir, crate::connection::ConnectionPool) {
        let tmp = TempDir::new().expect("tmpdir");
        let pool = open(&tmp.path().join("t.sqlite")).expect("open");
        (tmp, pool)
    }

    fn sample_account() -> Account {
        Account {
            id: Id::<AccountTag>::from_ulid(Ulid::from(1u128 << 64)),
            kind: AccountKind::Personal,
            upn: "alice@example.com".into(),
            tenant_id: "9188040d-6c67-4c5b-b112-36a304b66dad".into(),
            drive_id: DriveId::new("drv-1"),
            display_name: "Alice".into(),
            keychain_ref: KeychainRef::new("kc-1"),
            scopes: vec!["Files.ReadWrite".into()],
            created_at: Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap()),
            updated_at: Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap()),
        }
    }

    #[test]
    fn upsert_then_get_round_trips() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let acct = sample_account();
        upsert(&conn, &acct).expect("upsert");
        let back = get(&conn, &acct.id).expect("get").expect("present");
        assert_eq!(back, acct);
    }

    #[test]
    fn get_returns_none_for_missing_id() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let id = Id::<AccountTag>::from_ulid(Ulid::new());
        assert!(get(&conn, &id).expect("get").is_none());
    }

    #[test]
    fn upsert_is_idempotent_with_updated_fields() {
        let (_tmp, pool) = fresh_db();
        let conn = pool.get().expect("conn");
        let mut a = sample_account();
        upsert(&conn, &a).expect("first");
        a.display_name = "Alice Updated".into();
        upsert(&conn, &a).expect("second");
        let back = get(&conn, &a.id).expect("get").expect("present");
        assert_eq!(back.display_name, "Alice Updated");
    }
}
```

Write the parallel set for pairs (insert account first, then a pair referencing it; round-trip; uniqueness constraint test that two pairs with the same local_path fail).

- [ ] **Step 4.5: Wire `queries` into `lib.rs`**

`lib.rs` already declares `pub mod queries;` via the stub from Task 1. No change needed.

- [ ] **Step 4.6: Gate + commit**

Expected: workspace test count grows by ~6 (3 accounts tests + 3 pairs tests). Total: 46.

Commit: `feat(state): accounts and pairs query helpers with serde round-trip`

---

## Task 5: file_entries + file_ops queries

**Files:**
- Create: `crates/onesync-state/src/queries/file_entries.rs`
- Create: `crates/onesync-state/src/queries/file_ops.rs`
- Modify: `crates/onesync-state/src/queries/mod.rs` to add the modules.

Follow the Task 4 template. Critical points:

- **`file_entries.local_json` / `remote_json` / `synced_json`** store the full `FileSide` as JSON via `serde_json::to_string`. Read back with `serde_json::from_str::<FileSide>(...)`.
- **`file_entries.kind` / `sync_state`** use string mappings (same pattern as `accounts.kind`).
- **`pending_op_id`** is nullable; store as `Option<String>`.
- **Primary key is `(pair_id, relative_path)`** — `upsert` uses `ON CONFLICT(pair_id, relative_path)`.
- **`file_entries_dirty`** uses the partial index — `SELECT ... WHERE pair_id = ? AND sync_state <> 'clean' ORDER BY updated_at LIMIT ?`.
- **`file_ops.last_error_json` / `metadata_json`** are nullable serde JSON. Same pattern.
- **`op_update_status`** is a focused `UPDATE file_ops SET status = ?, started_at = COALESCE(started_at, ?), finished_at = ? WHERE id = ?`. Set `started_at` when transitioning to `in_progress`, set `finished_at` when transitioning to `success` or `failed`. The caller passes the current timestamp through the `Clock` port — for this query, accept an `&Timestamp` parameter.

Expose:
- `file_entries::{upsert, get, dirty}` where `dirty(conn, pair_id, limit) -> Vec<FileEntry>`.
- `file_ops::{insert, update_status, in_flight}`.

Test set per module: round-trip, missing-id, partial-index check (dirty returns only non-clean rows), serde encoding of `FileSide` fields.

Expected total tests after Task 5: 53–55.

Commit: `feat(state): file_entries and file_ops query helpers`

---

## Task 6: conflicts + sync_runs + audit_events queries

**Files:**
- Create: `crates/onesync-state/src/queries/conflicts.rs`
- Create: `crates/onesync-state/src/queries/sync_runs.rs`
- Create: `crates/onesync-state/src/queries/audit.rs`
- Modify: `crates/onesync-state/src/queries/mod.rs`

Conflicts: `insert(conn, &Conflict)` and `unresolved(conn, pair_id) -> Vec<Conflict>`. The two `FileSide` columns serialise to JSON; the partial index `conflicts_pair_unresolved_idx` is what `unresolved` exploits (`WHERE resolved_at IS NULL`).

Sync runs: `record(conn, &SyncRun)` (insert or replace, depending on whether the run has finished). `recent(conn, pair_id, limit) -> Vec<SyncRun>` (ordered by `started_at DESC`).

Audit: `append(conn, &AuditEvent)` and `recent(conn, limit) -> Vec<AuditEvent>` and `search(conn, from_ts, to_ts, level?, pair_id?, limit) -> Vec<AuditEvent>`.

Tests:
- Conflict insert + query unresolved.
- Sync-run record + recent ordering.
- Audit append + tail ordering (recent first).
- Audit search by time window.

Expected tests after Task 6: ~62.

Commit: `feat(state): conflicts, sync_runs, and audit query helpers`

---

## Task 7: instance_config queries

**Files:**
- Create: `crates/onesync-state/src/queries/config.rs`
- Modify: `crates/onesync-state/src/queries/mod.rs`

`instance_config` is a singleton row with `id = 1`. Operations:

```rust
pub fn get(conn: &Connection) -> Result<InstanceConfig, StateStoreError> {
    let cfg = conn.query_row(
        "SELECT log_level, notify, allow_metered, min_free_gib, updated_at \
         FROM instance_config WHERE id = 1",
        [],
        row_to_config,
    ).optional()
     .map_err(|e| StateStoreError::Sqlite(e.to_string()))?;
    cfg.ok_or_else(|| StateStoreError::Sqlite("instance_config row missing".into()))
}

pub fn upsert(conn: &Connection, cfg: &InstanceConfig) -> Result<(), StateStoreError> { ... }

/// Insert the singleton with default values if absent. Called on first open.
pub fn ensure_present(conn: &Connection, now: &Timestamp) -> Result<(), StateStoreError> { ... }
```

`ensure_present` is invoked from `connection::open` immediately after migrations. Adjust `open` to call it. Choice of `Timestamp` for the default-row `updated_at` comes from `chrono::Utc::now()` — that's a workspace-level disallowed method. Use `chrono::Utc::now()` here with `#[allow(clippy::disallowed_methods)]` + `// LINT:` justifying it as "first-run bootstrap, before the Clock port is wired", OR pull the timestamp through the call site (preferred — make `open` accept a `&dyn Clock` or `&Timestamp`).

Pick the latter — pass a `&Timestamp` into `ensure_present`, and have `open` accept an optional `now: Timestamp` argument. Update Task 3's tests to pass a fixed timestamp from `chrono::TestClock`-equivalent fixtures.

Tests:
- Default row inserted on first open.
- `set` updates the row, leaves `id` at 1.
- `get` on a fresh-but-not-bootstrapped db returns an error (regression for the case where migration ran but `ensure_present` didn't).

Expected after Task 7: ~65 tests.

Commit: `feat(state): instance_config singleton with default-row bootstrap`

---

## Task 8: `SqliteStore` implements `StateStore`

**Files:**
- Modify: `crates/onesync-state/src/store.rs` (replace stub)

This is the assembly task. `SqliteStore` holds a `ConnectionPool`; its `impl StateStore for SqliteStore` dispatches every trait method to the corresponding `queries::<module>::<fn>` helper inside a `spawn_blocking` to keep the async trait honest (rusqlite is sync).

- [ ] **Step 8.1: Implement `SqliteStore`**

```rust
//! `SqliteStore` — concrete `StateStore` adapter.

use async_trait::async_trait;

use onesync_core::ports::{StateError, StateStore};
use onesync_protocol::{
    account::Account,
    audit::AuditEvent,
    conflict::Conflict,
    enums::FileOpStatus,
    file_entry::FileEntry,
    file_op::FileOp,
    id::{AccountId, FileOpId, PairId},
    pair::Pair,
    path::RelPath,
    sync_run::SyncRun,
};

use crate::connection::ConnectionPool;
use crate::error::StateStoreError;
use crate::queries;

/// SQLite-backed `StateStore` adapter.
#[derive(Clone)]
pub struct SqliteStore {
    pool: ConnectionPool,
}

impl SqliteStore {
    /// Construct a `SqliteStore` backed by the given pool.
    #[must_use]
    pub fn new(pool: ConnectionPool) -> Self {
        Self { pool }
    }
}

fn map_err(e: StateStoreError) -> StateError {
    match e {
        StateStoreError::Sqlite(s) => StateError::Io(s),
        StateStoreError::Migration(s) | StateStoreError::Schema(s) => StateError::Schema(s),
    }
}

#[async_trait]
impl StateStore for SqliteStore {
    async fn account_upsert(&self, account: &Account) -> Result<(), StateError> {
        let pool = self.pool.clone();
        let account = account.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::accounts::upsert(&conn, &account).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn account_get(&self, id: &AccountId) -> Result<Option<Account>, StateError> {
        let pool = self.pool.clone();
        let id = id.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::accounts::get(&conn, &id).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    // ... pattern repeats for every method:
    //   1. clone pool + arguments
    //   2. spawn_blocking
    //   3. acquire connection, dispatch to queries module
    //   4. join + map join error to StateError::Io
}
```

Implement every remaining method (`pair_upsert`, `pair_get`, `pairs_active`, `file_entry_upsert`, `file_entry_get`, `file_entries_dirty`, `run_record`, `op_insert`, `op_update_status`, `conflict_insert`, `conflicts_unresolved`, `audit_append`) by the same pattern. Each one is ~10 lines.

- [ ] **Step 8.2: Integration test — full pipeline against tempfile db**

Create `crates/onesync-state/tests/integration_pipeline.rs`:

```rust
//! End-to-end smoke test: open a fresh db, insert an account + pair, query back, exercise the
//! file-entries dirty index, record a run + ops, insert and resolve a conflict.

use onesync_core::ports::StateStore;
use onesync_protocol::{ /* … */ };
use onesync_state::{open, SqliteStore};
use tempfile::TempDir;

#[tokio::test]
async fn full_pipeline_round_trips() {
    let tmp = TempDir::new().expect("tmpdir");
    let pool = open(&tmp.path().join("t.sqlite")).expect("open");
    let store = SqliteStore::new(pool);

    // construct sample Account, Pair, FileEntry, FileOp, Conflict using TestClock + TestIdGenerator
    // from onesync_time::fakes; upsert each; query back; assert equality.
    // Then mark a file entry dirty, verify it appears in `file_entries_dirty`.

    // (Write the actual fixture and assertions; do not stub.)
}
```

Use `onesync_time::fakes::{TestClock, TestIdGenerator}` for determinism. The test should exercise every trait method at least once.

- [ ] **Step 8.3: Gate + commit**

Expected total tests: ~70+.

Commit: `feat(state): SqliteStore implements the StateStore port end-to-end`

---

## Task 9: In-memory fake `StateStore`

**Files:**
- Modify: `crates/onesync-state/src/fakes.rs` (replace stub)

The fake lives in `crates/onesync-state/src/fakes.rs` behind `#[cfg(any(test, feature = "fakes"))]`. Add the `fakes` feature to `Cargo.toml`. The fake stores entities in `Mutex<HashMap>` collections — no async I/O, no transactions.

- [ ] **Step 9.1: Implement `InMemoryStore`**

```rust
//! In-memory `StateStore` for engine tests.

#![cfg(any(test, feature = "fakes"))]
#![allow(clippy::expect_used)] // LINT: mutex-poison standard pattern, see onesync-time/fakes

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use onesync_core::ports::{StateError, StateStore};
use onesync_protocol::{ /* every entity */ };

/// In-memory `StateStore` for use in engine tests.
#[derive(Default, Debug)]
pub struct InMemoryStore {
    accounts: Mutex<HashMap<AccountId, Account>>,
    pairs:    Mutex<HashMap<PairId, Pair>>,
    file_entries: Mutex<HashMap<(PairId, RelPath), FileEntry>>,
    file_ops: Mutex<HashMap<FileOpId, FileOp>>,
    conflicts: Mutex<HashMap<ConflictId, Conflict>>,
    sync_runs: Mutex<HashMap<SyncRunId, SyncRun>>,
    audit: Mutex<Vec<AuditEvent>>,
}

#[async_trait]
impl StateStore for InMemoryStore {
    // straightforward locked HashMap operations; no spawn_blocking needed
    // (the fake doesn't block the runtime)
}
```

Important: behaviour must match `SqliteStore` for the engine's view of the world. Specifically:
- `pairs_active` excludes `Removed`.
- `file_entries_dirty` excludes `Clean`.
- `conflicts_unresolved` excludes those with `resolved_at` set.
- `op_update_status` preserves `started_at` if already set.

- [ ] **Step 9.2: Cross-store parity test**

Write a parameterised test that runs the same sequence of operations against `SqliteStore` and `InMemoryStore` and asserts the outputs match. Use `proptest` if scope allows (defer to a future task if it complicates the implementation; a hand-coded scenario test is sufficient for M2).

- [ ] **Step 9.3: Gate + commit**

Commit: `feat(state): in-memory StateStore fake plus parity test against SQLite`

---

## Task 10: Retention/compaction + xtask schema dump

**Files:**
- Modify: `crates/onesync-state/src/retention.rs`
- Create: `xtask/Cargo.toml`
- Create: `xtask/src/main.rs`
- Modify: workspace `Cargo.toml` to add `xtask` to `members`.
- Create: `crates/onesync-state/schema.sql` (generated by the xtask; checked in)

Retention does one pass per call:

```rust
/// Prune rows past the retention window. Idempotent.
pub fn run(
    conn: &mut rusqlite::Connection,
    now: &Timestamp,
) -> Result<RetentionReport, StateStoreError> { ... }
```

Reads the four windows from `onesync_core::limits` (`AUDIT_RETENTION_DAYS`, `RUN_HISTORY_RETENTION_DAYS`, `CONFLICT_RETENTION_DAYS`, plus the 7-day window for `pairs.status='removed'`). Returns counts of rows pruned per table.

After retention, runs `PRAGMA optimize` (cheap; safe). Does NOT run `VACUUM` (too disruptive; spec defers to a weekly out-of-band job).

The `xtask` crate is a small `cargo run -p xtask -- dump-schema` helper that opens an in-memory db, applies migrations, and writes the resulting schema to `crates/onesync-state/schema.sql`. CI runs this and checks for divergence (added in Task 21).

Tests:
- Retention prunes expected rows; idempotent on second run.
- Soft-deleted pair (`status='removed'`) past 7 days is hard-deleted; cascade removes its `file_entries`, `file_ops`, `conflicts`, `sync_runs`.

Commit: `feat(state): retention compaction job and schema-dump xtask`

---

# Phase B — `onesync-fs-local`

## Task 11: Crate skeleton + deps

**Files:**
- Create: `crates/onesync-fs-local/Cargo.toml`
- Create: `crates/onesync-fs-local/src/lib.rs`
- Module stubs for `path`, `hash`, `write`, `ops`, `scan`, `watcher`, `volumes`, `adapter`, `error`, `fakes`.

`Cargo.toml`:

```toml
[package]
name = "onesync-fs-local"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
onesync-core     = { path = "../onesync-core" }
onesync-protocol = { path = "../onesync-protocol" }
async-trait      = { workspace = true }
blake3           = { workspace = true }
fs2              = { workspace = true }
notify           = { workspace = true }
tempfile         = { workspace = true }
thiserror        = { workspace = true }
tokio            = { workspace = true }

[dev-dependencies]
chrono           = { workspace = true }
proptest         = { workspace = true }
```

`lib.rs`:

```rust
//! macOS filesystem adapter implementing the `LocalFs` port.
//!
//! See [`docs/spec/05-local-adapter.md`](../../../../docs/spec/05-local-adapter.md).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod adapter;
pub mod error;
pub mod fakes;
pub mod hash;
pub mod ops;
pub mod path;
pub mod scan;
pub mod volumes;
pub mod watcher;
pub mod write;

pub use adapter::LocalFsAdapter;
pub use error::LocalFsAdapterError;
```

Stubs for each module match the Task 1 pattern.

Commit: `feat(fs-local): onesync-fs-local crate skeleton`

---

## Task 12: Path canonicalisation helpers

**Files:**
- Modify: `crates/onesync-fs-local/src/path.rs`

The `RelPath` and `AbsPath` newtypes already enforce static validation (NFC, byte cap, leading-slash, `..` rejection). This module adds runtime helpers:

```rust
/// Resolve `path` against `root`, returning a relative path within the pair root,
/// or `Err` if `path` escapes the root (symlink trickery, `..` resolution, etc.).
pub fn relativise(root: &AbsPath, path: &AbsPath) -> Result<RelPath, LocalFsAdapterError> { ... }

/// Compose an absolute path from a pair root and a relative path.
pub fn absolutise(root: &AbsPath, rel: &RelPath) -> AbsPath { ... }

/// Determine whether two paths live on the same filesystem volume (for cross-volume
/// rename detection).
pub fn same_volume(a: &Path, b: &Path) -> bool { ... }
```

Tests cover: relativise against the root, relativise outside the root → error, relativise with a `..` component → error (defence in depth), `same_volume` against tempdir + `/tmp` (or whatever heuristic — could use `nix::sys::stat::stat` or `std::fs::metadata().dev()`; pick the simplest portable option that works on macOS).

Commit: `feat(fs-local): runtime path helpers (relativise, absolutise, same-volume)`

---

## Task 13: BLAKE3 streaming hash with race detection

**Files:**
- Modify: `crates/onesync-fs-local/src/hash.rs`

```rust
//! BLAKE3 content-hash helper.

use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use onesync_core::limits::HASH_BLOCK_BYTES;
use onesync_protocol::primitives::ContentHash;

use crate::error::LocalFsAdapterError;

/// Compute the BLAKE3 digest of the file at `path`, streaming in `HASH_BLOCK_BYTES` chunks.
///
/// Returns `LocalFsAdapterError::Raced` if the file's `mtime` changed between the open and
/// the final read — that's how the engine detects a concurrent write underneath the hasher.
pub fn hash(path: &Path) -> Result<ContentHash, LocalFsAdapterError> {
    let mut file = File::open(path).map_err(LocalFsAdapterError::from)?;
    let mtime_before = file.metadata().map_err(LocalFsAdapterError::from)?.modified().ok();
    let mut buf = vec![0u8; HASH_BLOCK_BYTES];
    let mut hasher = blake3::Hasher::new();
    loop {
        let n = file.read(&mut buf).map_err(LocalFsAdapterError::from)?;
        if n == 0 { break; }
        hasher.update(&buf[..n]);
    }
    let mtime_after = file.metadata().map_err(LocalFsAdapterError::from)?.modified().ok();
    if mtime_before != mtime_after {
        return Err(LocalFsAdapterError::Raced);
    }
    let bytes: [u8; 32] = *hasher.finalize().as_bytes();
    Ok(ContentHash::from_bytes(bytes))
}
```

Run inside `spawn_blocking` from the adapter (Task 18).

Tests:
- Hash matches `b3sum` for a fixed-content fixture file.
- Hash of an empty file = BLAKE3 of "".
- Race detection: write to the file between open and final read, assert `Raced`.

Commit: `feat(fs-local): BLAKE3 streaming hash with mtime-race detection`

---

## Task 14: Atomic write

**Files:**
- Modify: `crates/onesync-fs-local/src/write.rs`

The recipe is: temp file in same dir, fsync, rename, dir-fsync. Use `tempfile::NamedTempFile::new_in(<parent>)` to land the temp file beside the target so `rename(2)` stays on the same inode. Hash the bytes as they pass through.

```rust
pub fn write_atomic(target: &Path, bytes: impl AsRef<[u8]>) -> Result<FileSide, LocalFsAdapterError> {
    let parent = target.parent().ok_or(LocalFsAdapterError::InvalidPath {
        reason: "target has no parent".into(),
    })?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(LocalFsAdapterError::from)?;

    use std::io::Write;
    let mut hasher = blake3::Hasher::new();
    let mut total: u64 = 0;
    for chunk in bytes.as_ref().chunks(HASH_BLOCK_BYTES) {
        tmp.write_all(chunk).map_err(LocalFsAdapterError::from)?;
        hasher.update(chunk);
        total += chunk.len() as u64;
    }
    tmp.as_file().sync_all().map_err(LocalFsAdapterError::from)?;

    // Atomically rename into place.
    tmp.persist(target).map_err(|e| LocalFsAdapterError::from(e.error))?;

    // fsync the directory so the rename is durable.
    let dir = std::fs::File::open(parent).map_err(LocalFsAdapterError::from)?;
    dir.sync_all().map_err(LocalFsAdapterError::from)?;

    let meta = std::fs::metadata(target).map_err(LocalFsAdapterError::from)?;
    let mtime = chrono::DateTime::<chrono::Utc>::from(
        meta.modified().map_err(LocalFsAdapterError::from)?
    );

    let bytes_hash: [u8; 32] = *hasher.finalize().as_bytes();
    Ok(FileSide {
        kind: FileKind::File,
        size_bytes: total,
        content_hash: Some(ContentHash::from_bytes(bytes_hash)),
        mtime: Timestamp::from_datetime(mtime),
        etag: None,
        remote_item_id: None,
    })
}
```

(For streaming writes too large to buffer in memory, add `write_atomic_stream(target, reader)` later; M2 only needs the byte-slice form to pass the engine's tests.)

Tests:
- Write small file; subsequent read returns identical bytes.
- Write to a path under a non-existent parent → error (no parent).
- Write to a path that already exists → file is replaced atomically; mid-write crash never leaves a partial file (simulate via dropping the temp file and asserting target untouched).
- Returned `FileSide` carries the BLAKE3 of the bytes and the post-write size.

Commit: `feat(fs-local): atomic write_atomic with temp+fsync+rename+dir-fsync`

---

## Task 15: Rename / delete / mkdir_p with cross-volume handling

**Files:**
- Modify: `crates/onesync-fs-local/src/ops.rs`

```rust
pub fn rename(from: &Path, to: &Path) -> Result<(), LocalFsAdapterError> {
    if crate::path::same_volume(from, to) {
        std::fs::rename(from, to).map_err(LocalFsAdapterError::from)
    } else {
        // Degrade: copy-then-delete. Surface CrossVolumeRename in the audit
        // but treat as success for the caller.
        std::fs::copy(from, to).map_err(LocalFsAdapterError::from)?;
        std::fs::remove_file(from).map_err(LocalFsAdapterError::from)?;
        Err(LocalFsAdapterError::CrossVolumeRename { method: "copy+delete" })
    }
}

pub fn delete(path: &Path) -> Result<(), LocalFsAdapterError> { ... }
pub fn mkdir_p(path: &Path) -> Result<(), LocalFsAdapterError> { ... }
```

(`CrossVolumeRename` is technically not an error — it's a successful degraded outcome. The adapter at Task 18 maps this to a successful return value + an audit emission via `AuditSink`. Discuss with the engine integration if the trait shape needs an `Ok(RenameOutcome::Degraded { method })` variant; if so, change the port now. For M2, keeping `CrossVolumeRename` as a recoverable error works.)

Tests cover: rename within tempdir, delete file, delete empty directory, delete non-empty directory → error, mkdir_p creates nested directories, mkdir_p on existing dir is a no-op.

Commit: `feat(fs-local): rename, delete, mkdir_p with cross-volume degradation`

---

## Task 16: Recursive scan with bounded BFS

**Files:**
- Modify: `crates/onesync-fs-local/src/scan.rs`

```rust
pub fn scan(root: &Path) -> Result<Vec<LocalFileMeta>, LocalFsAdapterError> {
    let mut queue: VecDeque<PathBuf> = VecDeque::with_capacity(SCAN_QUEUE_DEPTH_MAX);
    queue.push_back(root.to_path_buf());
    let mut out = Vec::new();
    while let Some(dir) = queue.pop_front() {
        for entry in std::fs::read_dir(&dir).map_err(LocalFsAdapterError::from)? {
            let entry = entry.map_err(LocalFsAdapterError::from)?;
            let meta = entry.metadata().map_err(LocalFsAdapterError::from)?;
            let name = entry.file_name();
            // Skip ._*, .DS_Store, Icon\r, .localized, symlinks (warn), too-long paths.
            if should_skip(&name, &meta) { continue; }
            let path = entry.path();
            if meta.is_dir() {
                if queue.len() >= SCAN_QUEUE_DEPTH_MAX {
                    return Err(LocalFsAdapterError::InvalidPath {
                        reason: "scan queue overflow".into(),
                    });
                }
                queue.push_back(path.clone());
                out.push(LocalFileMeta::dir(path, &meta));
            } else if meta.is_file() {
                out.push(LocalFileMeta::file(path, &meta));
            }
        }
    }
    Ok(out)
}
```

`LocalFileMeta` is a local struct holding `{ path: PathBuf, kind: FileKind, size: u64, mtime: Timestamp }`. Don't hash during scan — hashing happens lazily during reconciliation.

Tests: scan a tempdir with mixed files/dirs/symlinks, count returned entries, confirm `.DS_Store` is excluded, confirm a depth-3 nested tree fully walks.

Commit: `feat(fs-local): bounded BFS scan with skip list`

---

## Task 17: FSEvents watcher

**Files:**
- Modify: `crates/onesync-fs-local/src/watcher.rs`

Wrap `notify::RecommendedWatcher` behind a `tokio::sync::mpsc` channel. The watcher's callback runs on the CoreFoundation runloop thread; pump events into the channel; surface `MustScanSubDirs` as an `Overflow` flag on the stream.

```rust
pub struct Watcher {
    _watcher: notify::RecommendedWatcher,
    rx: tokio::sync::mpsc::Receiver<LocalEvent>,
}

pub fn watch(root: &Path) -> Result<Watcher, LocalFsAdapterError> { ... }

pub enum LocalEvent {
    Created(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
    Renamed { from: PathBuf, to: PathBuf },
    Overflow,
    Unmounted,
}
```

Buffer depth = `FSEVENT_BUFFER_DEPTH`. When the channel is full, set the `Overflow` flag and drop the in-flight event.

Tests:
- Create a tempdir, start a watcher, touch a file, assert a `Created` event arrives within 1s.
- Modify the file, assert `Modified`.
- Delete the file, assert `Deleted`.
- Rename, assert `Renamed`.
- Overflow simulation: spam events faster than the consumer can drain; assert `Overflow` arrives at least once.

Commit: `feat(fs-local): FSEvents watcher with overflow handling`

---

## Task 18: `LocalFsAdapter` assembles the port

**Files:**
- Modify: `crates/onesync-fs-local/src/adapter.rs`

Compose the above modules into the `LocalFs` trait implementation. Every method wraps blocking I/O in `spawn_blocking`.

```rust
pub struct LocalFsAdapter;

#[async_trait]
impl LocalFs for LocalFsAdapter {
    async fn scan(&self, root: &AbsPath) -> Result<LocalScanStream, LocalFsError> { ... }
    async fn read(&self, path: &AbsPath) -> Result<LocalReadStream, LocalFsError> { ... }
    async fn write_atomic(&self, path: &AbsPath, stream: LocalWriteStream) -> Result<FileSide, LocalFsError> { ... }
    async fn rename(&self, from: &AbsPath, to: &AbsPath) -> Result<(), LocalFsError> { ... }
    async fn delete(&self, path: &AbsPath) -> Result<(), LocalFsError> { ... }
    async fn mkdir_p(&self, path: &AbsPath) -> Result<(), LocalFsError> { ... }
    async fn watch(&self, root: &AbsPath) -> Result<LocalEventStream, LocalFsError> { ... }
    async fn hash(&self, path: &AbsPath) -> Result<ContentHash, LocalFsError> { ... }
}
```

The placeholder `LocalScanStream`/`LocalReadStream`/`LocalWriteStream`/`LocalEventStream` types from M1 stay; flesh them out as concrete types inside the adapter crate. They will be Tokio streams wrapping the BFS scan, the file reader, the byte buffer (or AsyncRead), and the watcher channel respectively.

Integration test: drive the adapter end-to-end — scan a tempdir, hash a file, atomic-write a new file, rename, delete, mkdir — using `tokio::test`.

Commit: `feat(fs-local): LocalFsAdapter implements the LocalFs port`

---

## Task 19: In-memory fake `LocalFs`

**Files:**
- Modify: `crates/onesync-fs-local/src/fakes.rs`

Engine tests need a `LocalFs` that doesn't touch the disk. Store a `HashMap<RelPath, FakeFile>` keyed by relative path; `FakeFile { bytes: Vec<u8>, mtime: Timestamp, kind: FileKind }`. Implement every `LocalFs` method against this. The watcher returns a programmable event stream (push events from test code via a test-only `inject_event` method).

Cross-fake parity test: round-trip the same op sequence through `LocalFsAdapter` (tempdir) and `InMemoryLocalFs`; results must match.

Commit: `feat(fs-local): in-memory LocalFs fake with programmable watcher`

---

# Phase C — Integration + CI + close

## Task 20: Cross-crate integration test

**Files:**
- Create: `tests/m2_integration.rs` (workspace-level integration test crate; add to workspace `members`)

End-to-end M2 acceptance test:

1. Create a tempdir + tempfile SQLite db.
2. Open `SqliteStore`.
3. Open `LocalFsAdapter` against the tempdir.
4. Synthetically create 100 files in the tempdir.
5. Scan via `LocalFsAdapter::scan`, capture entries.
6. For each scanned entry, hash via `LocalFsAdapter::hash`.
7. Upsert one `Pair` and a `FileEntry` per scanned file via `SqliteStore`.
8. Query back via `SqliteStore::file_entries_dirty` (returns 0 — all are `Clean`).
9. Modify one file on disk; trigger a watcher event; the test re-hashes it and observes the hash changed.
10. Upsert the `FileEntry` with `sync_state = Dirty`; query `file_entries_dirty`; assert exactly 1 row.

This test exercises both crates against real backends and proves the M2 milestone end-to-end.

Commit: `test(workspace): M2 cross-crate scan + state-store integration test`

---

## Task 21: GitHub Actions CI

**Files:**
- Create: `.github/workflows/ci.yml`

Single job, macos-latest runner. Steps:

1. Checkout (with `fetch-depth: 0`).
2. Install Rust 1.95.0 via `dtolnay/rust-toolchain@stable` (or pin to the action's tag).
3. Cache `~/.cargo` and `target/`.
4. Install `cargo-nextest` (`cargo install cargo-nextest --locked` — cached).
5. `cargo fmt --all -- --check`.
6. `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
7. `cargo nextest run --workspace --no-fail-fast`.
8. `cargo deny check` (optional in this task; defer to M3 if `cargo-deny` isn't already on the runner).
9. `cargo run -p xtask -- check-schema` — fails CI if the checked-in `schema.sql` diverges from what migrations produce.

Triggers: `push` to `main`, and `pull_request` to `main`.

Commit: `ci: macOS gate workflow (fmt, clippy, nextest, schema parity)`

---

## Task 22: Workspace gate + advance `main` + push

**Files:** none.

- [ ] **Step 22.1: Run the full gate one last time**

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
```

- [ ] **Step 22.2: Advance `main` and push**

```
jj bookmark move main --to @-
jj git push
```

- [ ] **Step 22.3: Update the roadmap**

Edit `docs/plans/2026-05-11-roadmap.md`'s M2 row: `Status: Complete (origin/main @ <sha>, 2026-MM-DD). N tests pass.`

Commit the roadmap update with subject `docs(plans): mark M2 complete on the roadmap` and push.

---

## Self-review checklist (after Task 22)

- [ ] `onesync-state` crate compiles and all queries have at least one round-trip test.
- [ ] `onesync-fs-local` crate compiles and the adapter exercises every port method.
- [ ] In-memory fakes match SQLite/disk behaviour in parity tests.
- [ ] Migration is idempotent; `schema.sql` and migrations agree (xtask `check-schema` passes).
- [ ] FSEvents overflow is surfaced as `LocalEvent::Overflow`, not silently dropped.
- [ ] Cross-volume rename degradation is observable via `LocalFsError::CrossVolumeRename`.
- [ ] Workspace test count is ≥ 80.
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` exits 0.
- [ ] `cargo fmt --all -- --check` exits 0.
- [ ] `origin/main` is at the final M2 commit.
- [ ] `MAX_RUNTIME_WORKERS` is added to `limits.rs` somewhere in M2 (the M1 final reviewer flagged this as a carry-over; the daemon doesn't exist yet but the constant should be present to satisfy spec parity). If you choose to defer to M5, document the deferral in the M2 retro section of the roadmap.

If any check fails, fix in place — do not declare M2 done.

---

## Exit and handoff

When the self-review is green:

- Update [`docs/plans/2026-05-11-roadmap.md`](2026-05-11-roadmap.md) M2 row to `Complete` with the merge commit SHA and test count.
- Open the M3 plan: `docs/plans/2026-MM-DD-m3-graph-adapter.md`. M3 only depends on M1, so it could have started in parallel — but we serialise here for simpler review. Use the writing-plans skill before any M3 task starts.
- Do **not** start M3 implementation tasks before the M3 plan exists.
