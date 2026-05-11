# 06 — State Store

**Status:** Draft · **Date:** 2026-05-11 · **Owner:** Stan

The state store is the daemon's durable memory: accounts, pairs, the `FileEntry` index,
in-flight `FileOp`s, conflict records, sync-run history, and audit events. It is a single
SQLite database living under the per-user state directory. The store is the truth for every
durable fact in onesync; tokens (which are not stored here) and runtime caches are the only
mutable state outside it.

The crate is `onesync-state`. It depends on `rusqlite` with the `bundled` and `serde_json`
features, plus `refinery` for migrations. The schema is documented here and is regenerated
into a sidecar `schema.sql` at build time.

---

## File layout

- Database: `<state-dir>/onesync.sqlite`
- Write-ahead log: `<state-dir>/onesync.sqlite-wal`
- Shared memory: `<state-dir>/onesync.sqlite-shm`
- Migrations applied: `schema_migrations` table within the db

`<state-dir>` is `${XDG_STATE_HOME:-$HOME/Library/Application Support}/onesync/` on macOS.
See [`08-installation-and-lifecycle.md`](08-installation-and-lifecycle.md) for the
authoritative path table.

The database is opened with:

- `PRAGMA journal_mode = WAL`
- `PRAGMA synchronous = NORMAL`
- `PRAGMA foreign_keys = ON`
- `PRAGMA busy_timeout = 5000`
- `PRAGMA temp_store = MEMORY`

Connections are managed by a `r2d2`-style pool of size `STATE_POOL_SIZE` (default 4). Reads
share the pool; writes serialise behind the single-writer SQLite contract.

---

## Tables

The schema below uses SQLite's typing affinity. Every table carries `created_at` and
`updated_at` as ISO-8601 UTC text. Primary keys are `TEXT` holding the typed-ID literal
(`pair_<ulid>`, `acct_<ulid>`, …).

### `accounts`

```sql
CREATE TABLE accounts (
  id              TEXT PRIMARY KEY,
  kind            TEXT NOT NULL CHECK (kind IN ('personal','business')),
  upn             TEXT NOT NULL,
  tenant_id       TEXT NOT NULL,
  drive_id        TEXT NOT NULL,
  display_name    TEXT NOT NULL,
  keychain_ref    TEXT NOT NULL,
  scopes_json     TEXT NOT NULL,            -- JSON array of strings
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL
);
CREATE UNIQUE INDEX accounts_upn_uq ON accounts(upn);
```

### `pairs`

```sql
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
```

### `file_entries`

```sql
CREATE TABLE file_entries (
  pair_id            TEXT NOT NULL REFERENCES pairs(id) ON DELETE CASCADE,
  relative_path      TEXT NOT NULL,
  kind               TEXT NOT NULL CHECK (kind IN ('file','directory')),
  sync_state         TEXT NOT NULL CHECK
                       (sync_state IN ('clean','dirty','pending_upload',
                                       'pending_download','pending_conflict','in_flight')),
  local_json         TEXT,                  -- JSON FileSide or null
  remote_json        TEXT,
  synced_json        TEXT,
  pending_op_id      TEXT REFERENCES file_ops(id) ON DELETE SET NULL,
  updated_at         TEXT NOT NULL,
  PRIMARY KEY (pair_id, relative_path)
);
CREATE INDEX file_entries_dirty_idx
  ON file_entries(pair_id, sync_state)
  WHERE sync_state <> 'clean';
```

The partial index on non-clean rows is the engine's main scan path; clean rows are the
overwhelming majority and they never appear in the index.

### `file_ops`

```sql
CREATE TABLE file_ops (
  id                 TEXT PRIMARY KEY,
  run_id             TEXT NOT NULL REFERENCES sync_runs(id) ON DELETE CASCADE,
  pair_id            TEXT NOT NULL REFERENCES pairs(id) ON DELETE CASCADE,
  relative_path      TEXT NOT NULL,
  kind               TEXT NOT NULL,         -- FileOpKind value
  status             TEXT NOT NULL,         -- FileOpStatus value
  attempts           INTEGER NOT NULL DEFAULT 0,
  last_error_json    TEXT,
  metadata_json      TEXT,                  -- e.g. upload session URL
  enqueued_at        TEXT NOT NULL,
  started_at         TEXT,
  finished_at        TEXT
);
CREATE INDEX file_ops_pair_status_idx ON file_ops(pair_id, status);
CREATE INDEX file_ops_run_idx ON file_ops(run_id);
```

### `conflicts`

```sql
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
  resolution             TEXT,              -- 'auto' | 'manual' | null while pending
  note                   TEXT
);
CREATE INDEX conflicts_pair_unresolved_idx
  ON conflicts(pair_id)
  WHERE resolved_at IS NULL;
```

### `sync_runs`

```sql
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
  outcome            TEXT,                  -- RunOutcome value once finished
  outcome_detail     TEXT
);
CREATE INDEX sync_runs_pair_started_idx ON sync_runs(pair_id, started_at DESC);
```

### `audit_events`

```sql
CREATE TABLE audit_events (
  id            TEXT PRIMARY KEY,
  ts            TEXT NOT NULL,
  level         TEXT NOT NULL CHECK (level IN ('info','warn','error')),
  kind          TEXT NOT NULL,
  pair_id       TEXT REFERENCES pairs(id) ON DELETE SET NULL,
  payload_json  TEXT NOT NULL
);
CREATE INDEX audit_events_ts_idx ON audit_events(ts DESC);
```

### `instance_config`

```sql
CREATE TABLE instance_config (
  id            INTEGER PRIMARY KEY CHECK (id = 1),
  log_level     TEXT NOT NULL DEFAULT 'info',
  notify        INTEGER NOT NULL DEFAULT 1,
  allow_metered INTEGER NOT NULL DEFAULT 0,
  min_free_gib  INTEGER NOT NULL DEFAULT 2,
  updated_at    TEXT NOT NULL
);
```

Singleton row guaranteed by the `CHECK` on the primary key.

### `schema_migrations`

Maintained by `refinery`. Holds the applied version vector.

---

## Migrations

Migrations are versioned, monotonic, and ship as embedded SQL files in `onesync-state`. The
first migration creates every table listed above; subsequent migrations add columns, add
indexes, or backfill data. The rules:

- A migration is **immutable** once shipped in a release. Fixes are forward migrations.
- A migration that requires data movement is split into two: one that adds a new column /
  table, one that backfills, one that drops the old column. Each ships in its own release.
- Every migration is wrapped in a transaction. If it fails the daemon refuses to start.
- The `schema_migrations` table is queried at startup and compared against the embedded
  migration set; if the db is newer than the binary (downgrade), the daemon refuses to start
  with `state.downgrade.refused`.

The schema sidecar `schema.sql` is regenerated by `cargo xtask dump-schema` and checked in;
CI verifies that the generated file matches what the migrations produce, so divergence is
caught.

---

## Secrets handling

The store does not persist any secret. The `accounts.keychain_ref` column holds an opaque
handle that the keychain adapter resolves to a refresh token. The handle itself is not
sensitive — it is the keychain `account` field — but the database is treated as if it were
sensitive for the purposes of file permissions:

- `<state-dir>` is `chmod 0700` at install time.
- `onesync.sqlite` and its WAL/SHM siblings are `chmod 0600`.
- The daemon refuses to open the db if the permission bits are looser; the message points
  the user at `onesync state repair-perms`.

---

## Backups and exports

The store supports two operations through the CLI:

- `onesync state backup --to <path>`: takes a consistent snapshot using SQLite's online
  backup API and writes it as a single `.sqlite` file plus a JSON manifest with version
  metadata.
- `onesync state export --to <dir>`: dumps every table as JSON Lines for human review. This
  is the format used for support escalations; it deliberately excludes any secret reference.

There is no `import` command. Recovery from a backup is performed by replacing the live
database file while the daemon is stopped; the daemon will re-validate on next start.

---

## Concurrency rules

- The daemon is the only writer. The CLI never opens the db; it makes RPCs.
- Reads are best-effort consistent within a single statement. The engine reads `FileEntry`
  rows inside the transaction that will update them.
- The engine's planning phase runs inside a single `BEGIN IMMEDIATE` transaction so the
  resulting `FileOp`s plus the `FileEntry.sync_state` updates appear atomically.
- `sync_runs`, `file_ops` updates, and `audit_events` inserts are short transactions; they
  do not nest under the planning transaction.

If two phases attempt to take an `IMMEDIATE` write lock simultaneously the second waits up
to `busy_timeout`. The engine treats `SQLITE_BUSY` as a programmer error (the single-pair
ownership rule prevents it) and refuses to retry silently.

---

## Retention and compaction

| Table | Default retention | Mechanism |
|---|---|---|
| `audit_events` | `AUDIT_RETENTION_DAYS` (30) | Daily compaction job deletes rows past the window. |
| `sync_runs` | `RUN_HISTORY_RETENTION_DAYS` (90) | Same job; cascade deletes `file_ops`. |
| `conflicts` (resolved) | `CONFLICT_RETENTION_DAYS` (180) | Same job. |
| `file_ops` (terminal) | tied to parent `sync_run` | Cascade from runs. |
| `file_entries` | until pair removed | Cleared on pair removal. |
| `pairs` (status='removed') | 7 days | Compaction; gives users a window to undo. |

After compaction the daemon runs `PRAGMA optimize` and, weekly, `VACUUM INTO` to a sibling
file and rotates it in (the live `VACUUM` would block writers for too long on large
databases).

---

## Assumptions and open questions

**Assumptions**

- SQLite WAL mode is reliable on macOS APFS. This is well-attested by other production
  software (Safari, mail clients, Things, etc.).
- A single SQLite file is large enough for the workload. Even at 100k files per pair, the
  database is in the tens of MiB.
- JSON-encoded `FileSide` columns are acceptable for storage. They are read by the engine
  only; the cost of a per-field column expansion is not justified.

**Decisions**

- *Single SQLite file.* **No per-pair database, no on-disk catalog separation.** Simpler
  backup, simpler schema evolution; partial indexes give us the hot-path performance.
- *`refinery` for migrations.* **Embedded SQL files versioned with the binary.** No external
  migration runner; the daemon owns its schema.
- *Partial index on non-clean `file_entries`.* **Keeps the hot index tiny.** Clean rows are
  ~99% of the table for any healthy pair.
- *No on-disk secret material in the state store.* **`keychain_ref` is the only pointer.**
  Compromising the SQLite file does not yield credentials.
- *Deletion paths are asymmetric by design.* **`pair.remove` soft-deletes (status = `removed`)
  and the row survives for 7 days; `account.remove --cascade-pairs` hard-deletes via the
  `pairs.account_id … ON DELETE CASCADE` foreign key.** Signing out is treated as a stronger
  intent than removing a single pair; the 7-day undo window applies only to per-pair
  removals. CLI surfaces this difference with a confirmation prompt on
  `account remove --cascade-pairs`.

**Open questions**

- *Large pair scaling.* We have not load-tested with >1M files per pair. The partial-index
  strategy is sound in principle; we want measurements before raising
  `MAX_FILES_PER_PAIR_HINT`.
- *Encrypted state at rest.* macOS FileVault covers the disk; we have not decided whether to
  add per-file encryption (e.g. SQLCipher). The `keychain_ref` indirection means at-rest
  encryption is mainly a confidentiality-at-rest argument for the audit events.
