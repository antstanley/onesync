# Review: State store (onesync-state) — 2026-05-15

## Scope
- `crates/onesync-state/src/lib.rs`
- `crates/onesync-state/src/store.rs`
- `crates/onesync-state/src/connection.rs`
- `crates/onesync-state/src/retention.rs`
- `crates/onesync-state/src/fakes.rs`
- `crates/onesync-state/src/error.rs`
- `crates/onesync-state/src/migrations/mod.rs`
- `crates/onesync-state/src/migrations/V001__initial_schema.sql`
- `crates/onesync-state/src/migrations/V002__m9_fields.sql`
- `crates/onesync-state/src/migrations/V003__webhook_notification_url.sql`
- `crates/onesync-state/src/queries/{mod,pairs,accounts,file_entries,file_ops,audit,conflicts,sync_runs,config}.rs`
- `crates/onesync-state/schema.sql`
- `crates/onesync-state/tests/` (contract assumptions only)

## Method
- `reasoning-semiformally` patch verification / fault-localisation templates were applied to:
  - The `VACUUM INTO` backup path (path-injection / quoting analysis)
  - Multi-statement write sequences (`op_insert` + `update file_entry` + `audit_append`)
  - Retention vs in-flight write race window
  - Migration history vs `schema.sql` divergence
- Lighter narrative reasoning used for invariant restating, index-coverage, and parity checks.

## Findings
(Filled in progressively below — most severe first.)

### F1. `VACUUM INTO` builds SQL with string formatting — quoting can corrupt or fail the backup — CONCERN
**Location:** `crates/onesync-state/src/store.rs:343`
**Severity:** CONCERN
**Summary:** `backup_to` formats the destination path directly into a single-quoted SQL literal; any single quote in the path (legal on macOS) breaks the SQL, and the API exposes a caller-controlled `&Path`.
**Evidence:**
```rust
async fn backup_to(&self, to: &std::path::Path) -> Result<(), StateError> {
    ...
    conn.execute(&format!("VACUUM INTO '{}'", to.display()), [])
```
The path arrives via the `StateStore::backup_to` trait method. Callers in the CLI/daemon ultimately pass a user-supplied destination (e.g. `onesync backup --to <path>`), so this is *not* internal-only input — it is a user-controlled file path.
**Reasoning:**
PREMISES:
- P1: `VACUUM INTO 'literal'` requires a single-quoted SQL literal (SQLite has no parameter binding for filenames here).
- P2: `Path::display()` returns the OS string verbatim; macOS filenames legitimately contain `'`.
- P3: SQLite escapes single quotes by doubling them (`''`).

EXECUTION TRACE:
- Path `/tmp/it's.db` → formatted SQL becomes `VACUUM INTO '/tmp/it's.db'`. SQLite parses up to the first `'`, errors with `unrecognized token` / syntax error → `backup_to` fails. No injection of *destructive* statements is possible because `VACUUM INTO` is the only verb and the rest of the string is not a statement separator — SQLite would still need to parse a second statement, and `conn.execute` rejects multi-statement input.
- A path like `/tmp/x'; ATTACH DATABASE 'y' AS z; --.db` produces a syntactically valid string for the literal but `rusqlite::Connection::execute` rejects multiple statements, returning `ExecuteReturnedResults` / `MultipleStatement`. So the practical impact is a *broken backup*, not data corruption — but a determined attacker with control of the path could potentially produce more subtle parsing surprises (e.g. unicode quote homoglyphs are not folded by SQLite, but bare `'` is the only concern).

REGRESSION CHECK:
- The CLI surface in `crates/onesync-cli` (not in scope) supplies the path. Even if input is currently restricted to a directory the daemon writes to, this is the kind of construction that becomes a real injection vector the moment requirements change.

**Suggested direction:** Use the `sqlite3_filename` mechanics: either escape the path by doubling `'` characters before interpolation, or — preferred — open the destination via `rusqlite::backup::Backup` (which takes a `&Path` directly and uses the C `sqlite3_backup_init` API). The Backup API also gives progress callbacks and avoids the WAL-flush implication of `VACUUM INTO`.

### F2. `op_insert` + `file_entry` `pending_op_id` are not co-ordinated atomically — CONCERN
**Location:** `crates/onesync-state/src/store.rs:155-164` (`op_insert`); `queries/file_ops.rs::insert`; `queries/file_entries.rs::upsert`
**Severity:** CONCERN (pending confirmation in `file_ops::insert` body — written below after reading)
**Summary:** `StateStore` exposes `op_insert`, `op_update_status`, `file_entry_upsert`, and `audit_append` as four independent async methods, each grabbing a *fresh* pool connection. The engine must call them sequentially; a crash between calls leaves `file_entries.pending_op_id` and `file_ops.id` out of sync.
**Evidence:** Every method in `store.rs` follows the pattern `let conn = pool.get()?; queries::X(&conn, ...)` with no `conn.transaction()` and no compound method (e.g. `enqueue_op` taking both `FileOp` and `FileEntry`). The `StateStore` trait has no `with_tx`/`atomic` primitive.
**Reasoning:**
PREMISES:
- P1: The spec invariant (`docs/spec/06-state-store.md`) is that `file_entries.pending_op_id` is non-NULL exactly while a corresponding `file_ops` row exists in a non-terminal status.
- P2: The trait splits writes into independent calls; only single-statement transactions exist at the query layer.
- P3: SQLite each `execute` is wrapped in its own implicit transaction.

EXECUTION TRACE:
- Engine wants to enqueue an upload: `op_insert(op)` (commits) → process crashes → recovery sees `file_ops` row with `status='pending'` but no `file_entries.pending_op_id` pointing at it (or vice-versa, depending on order).
- This is "almost-atomic" — verifying which side the engine actually writes first matters but the trait *cannot* guarantee atomicity regardless of caller discipline.

**Suggested direction:** Either (a) add a `StateStore::enqueue_op(&FileOp, &FileEntryPatch)` method that wraps both writes in `conn.transaction()`, or (b) document the invariant as "engine reconciles on startup" and add a reconciliation query (find orphan ops / orphan pending_op_ids).

### F3. `retention::run` is not wrapped in a transaction — partial pruning on mid-run error — CONCERN
**Location:** `crates/onesync-state/src/retention.rs:33-89`
**Severity:** CONCERN
**Summary:** Four `DELETE` statements + `PRAGMA optimize` are issued back-to-back without a containing transaction. If the second `DELETE` fails (e.g. `SQLITE_BUSY` after timeout), the first one is already committed.
**Evidence:** `conn.execute("DELETE FROM audit_events ...")` then `conn.execute("DELETE FROM sync_runs ...")` etc. No `conn.transaction()`/`BEGIN`/`COMMIT` wrapping.
**Reasoning:**
- The function is documented as "Idempotent" — that property holds *as long as the second invocation runs to completion*, but the first invocation may leave the database in a state where retention partially fired. The `RetentionReport` returned mid-failure already counts deleted rows but the caller sees only an `Err`.
- The bigger issue: `DELETE FROM sync_runs ...` triggers `ON DELETE CASCADE` on `file_ops` (and on `file_entries` via FK chain). A failure here while `audit_events` is already deleted produces no actual corruption (FKs are still consistent), but recovery semantics differ from "transaction".
- `PRAGMA optimize` is a read of stats — failing it should not affect any DELETE.

**Suggested direction:** Wrap in `let tx = conn.transaction()?;` … `tx.commit()?;`. Single fsync (`synchronous=NORMAL`) per pass is cheaper than four implicit transactions.

### F4. `compact_now` runs `VACUUM` while holding a single pooled connection — blocks all writers for the full vacuum window — CONCERN
**Location:** `crates/onesync-state/src/store.rs:351-363`
**Severity:** CONCERN
**Summary:** `VACUUM` in SQLite acquires an exclusive lock and rewrites the entire database. While running, every other pool connection blocks for up to `busy_timeout = 5_000 ms` and then returns `SQLITE_BUSY`. On a large database the vacuum can run for tens of seconds.
**Evidence:** `conn.execute("VACUUM", [])` after `retention::run`. Pool has `STATE_POOL_SIZE = 4` connections; the other three are blocked on the exclusive lock.
**Reasoning:**
- `retention.rs` documents *"Does NOT run VACUUM (too disruptive)"* but `store.rs::compact_now` re-introduces it. The crate is internally contradictory about whether `VACUUM` is acceptable.
- `synchronous=NORMAL` doesn't help; the lock is page-level.
- The CLI presumably invokes `compact_now` on demand. Any concurrent sync write within the vacuum window will see `StateError::Io("busy")` and likely bubble up.

**Suggested direction:** Either (a) only invoke `VACUUM` when the daemon is quiesced (acquire a `Mutex` in `SqliteStore` that all writers hold for the duration), or (b) document this as a stop-the-world operation and route it through a control-plane command that pauses sync first.

### F5. `file_entries::dirty` ORDER BY `updated_at` is unsupported by the existing index — CONCERN
**Location:** `crates/onesync-state/src/queries/file_entries.rs:78-103`; partial index defined at `schema.sql:62-64` and `V001__initial_schema.sql:88-90`
**Severity:** CONCERN (perf, not correctness)
**Summary:** The partial index `file_entries_dirty_idx ON file_entries(pair_id, sync_state) WHERE sync_state <> 'clean'` does not cover `updated_at`. The query plan will be `SCAN file_entries USING INDEX file_entries_dirty_idx` followed by `USE TEMP B-TREE FOR ORDER BY` — i.e. a sort over all dirty entries for the pair.
**Evidence:**
```sql
SELECT ... FROM file_entries
WHERE pair_id = ? AND sync_state <> 'clean'
ORDER BY updated_at
LIMIT ?
```
**Reasoning:** The intent of the partial index is to make "dirty" lookups cheap. Without `updated_at` as a trailing index column the engine must materialise the full dirty set before truncating to `LIMIT`. For a pair with many pending entries this scales linearly with the dirty set, not with `limit`. The dirty index would still work correctly without ordering — the question is whether the engine relies on age-ordered processing. The async port doc-comment says *"ordered by `updated_at`"* so yes.
**Suggested direction:** Extend the partial index to `(pair_id, updated_at) WHERE sync_state <> 'clean'`. Drop `sync_state` from the index columns (the partial WHERE already prunes clean rows). This makes `ORDER BY updated_at LIMIT` a direct index range scan.

### F6. `audit::search` filtered queries are not index-covered when level / pair filters are present — NIT
**Location:** `crates/onesync-state/src/queries/audit.rs:72-120`
**Severity:** NIT (perf)
**Summary:** Only `audit_events_ts_idx ON audit_events(ts DESC)` exists. The `search` query filters by `ts BETWEEN ... AND level = ? AND pair_id = ?` but no composite index covers `(level, ts)` or `(pair_id, ts)`. With AUDIT_RETENTION_DAYS = 30 the table stays bounded but a 30-day window over a noisy pair becomes a full window scan with filter evaluation.
**Suggested direction:** Add `audit_events_pair_ts_idx ON audit_events(pair_id, ts DESC) WHERE pair_id IS NOT NULL` if pair-filtered search is common. Not urgent.

### F7. `StateStoreError::Sqlite` is a `String` — distinguishing constraint / busy / corruption costs information — NIT
**Location:** `crates/onesync-state/src/error.rs`
**Severity:** NIT
**Summary:** All SQLite failures collapse into `Sqlite(String)`. The port-level `StateError::Io(s)` is then a flat string. Callers cannot distinguish:
  - `SQLITE_BUSY` (worth retrying) vs `SQLITE_CONSTRAINT` (logic bug) vs `SQLITE_CORRUPT` (operator alert).
  - r2d2 pool exhaustion (`StateStoreError::Sqlite(format!("pool: {e}"))`) vs underlying query failure.
**Reasoning:** Every `map_err(|e| StateStoreError::Sqlite(e.to_string()))` discards the structured `rusqlite::Error` variant. The daemon will have to substring-match the string to decide whether to retry. That coupling between log format and control flow is fragile.
**Suggested direction:** Promote `StateStoreError::Sqlite` to carry the `rusqlite::ErrorCode` (or `rusqlite::Error` directly), and add `Busy`, `Constraint`, `Corruption`, `PoolExhausted` variants. The port-level `StateError` can stay narrow but at least gain a `Retryable(bool)` accessor.

### F8. `pairs::list` builds dynamic SQL by concatenation — safe today, fragile pattern — NIT
**Location:** `crates/onesync-state/src/queries/pairs.rs:84-115`; same pattern in `crates/onesync-state/src/queries/audit.rs:79-120`
**Severity:** NIT
**Summary:** Both functions construct SQL with `String::push_str` based on `Option` arguments. The values themselves are bound via `?`, so this is *not* SQL injection — but the pattern is one refactor away from someone adding `sql.push_str(&format!(" AND foo = '{}'", user_input))`.
**Reasoning:** The current code is correct: every literal goes through `params![…]`. The `Box<dyn ToSql>` dance in `audit::search` and `pairs::list` is awkward but sound. Concern is purely defensive: prefer building both branches as separate const strings (small, finite combinatorial space — `pairs::list` has only 4 combinations: account×include_removed) so the SQL is always a compile-time literal.
**Suggested direction:** Either replace with a static dispatch (4 prepared statements for pairs::list) or use a query builder crate; otherwise add a `// SECURITY:` comment explaining why `push_str` is safe here.

### F9. `r2d2_sqlite::SqliteConnectionManager::with_init` runs PRAGMAs per-connection — correct; but `foreign_keys` is a per-connection PRAGMA the test path can miss — NIT
**Location:** `crates/onesync-state/src/connection.rs:50-57`
**Severity:** NIT (test parity)
**Summary:** `with_init` runs the five PRAGMAs (`journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`, `temp_store=MEMORY`) every time a new connection is created — that's correct for SQLite where PRAGMAs are per-connection. **But** tests that bypass `open()` and use `rusqlite::Connection::open_in_memory()` directly (e.g. `retention.rs::retention_works_with_raw_connection` and `config.rs::get_returns_none_on_fresh_db_before_ensure_present`, plus `migrations/mod.rs::migrations_apply_to_fresh_memory_db`) only manually set `foreign_keys=ON` in *one* of those tests. The migrations module test does not set FKs at all, so any FK-relying assertion in that path is silently disabled.
**Reasoning:** The `migrations_apply_to_fresh_memory_db` test only asserts table names, so the omission is benign. But future tests added to that file would silently lose FK enforcement, masking schema bugs.
**Suggested direction:** Extract a `fn with_pragmas(conn: &Connection)` helper and call it from both `connection::open` (inside `with_init`) and every raw-connection test path.

### F10. ULID monotonicity is not relied upon for cross-row ordering — informational
**Location:** all `queries/*.rs`
**Severity:** Informational (no action)
**Summary:** No query orders by `id` *to recover chronological order*. The only `ORDER BY id` is in `accounts::list` and `pairs::list`/`pairs::active`, where the semantics are "stable listing" rather than "chronological". Time-ordered queries use `ts`, `started_at`, `updated_at`, `detected_at`, or `enqueued_at`. Therefore ULID monotonicity inside `onesync-time` is not load-bearing for correctness of this crate. (The reviewer's prompt flagged this; verifying it: no `ORDER BY id` query is on a history-growing table where chronological order matters.)

### F11. `update_status` wipes `finished_at` on transition to `Enqueued`/`Backoff`/`InProgress` — by design — informational
**Location:** `crates/onesync-state/src/queries/file_ops.rs:58-87`
**Severity:** Informational
**Summary:** The SQL `finished_at = ?` (not `COALESCE`) means any transition to a non-terminal status nulls out `finished_at`. This is intentional (a retried op shouldn't claim it finished previously) and the doc-comment matches, but worth pinning down: the test `update_status_preserves_started_at_across_backoff` covers `started_at` but no test verifies `finished_at` is cleared on retry. Add a regression test.

### F12. `account_remove` uses `DELETE FROM accounts WHERE id = ?` — relies on FK cascade across four tables; one is `ON DELETE SET NULL` — CONCERN
**Location:** `crates/onesync-state/src/queries/accounts.rs:78-82`; FK definitions in `V001__initial_schema.sql`
**Severity:** CONCERN
**Summary:** Deleting an account cascades to `pairs` (CASCADE) → `sync_runs` (CASCADE) → `file_ops` (CASCADE), `file_entries` (CASCADE), `conflicts` (CASCADE). However `audit_events.pair_id REFERENCES pairs(id) ON DELETE SET NULL`. The result: when an account is removed, all audit events touching that account's pairs lose their pair association. The audit trail therefore stops being queryable by pair-id for events involving the deleted account.
**Reasoning:**
- This is by design (otherwise audit history would be deleted), but it means `audit::search(pair=...)` will *silently* miss historical events after an account removal — they still exist but with `pair_id = NULL`.
- Combined with **F3** (no retention transaction) and the retention deleting `pairs WHERE status='removed'`, there's a path where: pair marked `removed` → audit events for that pair persist → 7 days later retention deletes the pair → cascade nulls those events' `pair_id`. Any reconstruction tooling that joins on `pair_id` loses the link.
**Suggested direction:** Document this explicitly in spec; or denormalise the pair display path into `audit_events.payload_json` at append time so historical context survives.

### F13. `file_entries.pending_op_id REFERENCES file_ops(id) ON DELETE SET NULL` — orphan visibility on retention — CONCERN
**Location:** `V001__initial_schema.sql:84`; interacts with `retention.rs`
**Severity:** CONCERN
**Summary:** When `sync_runs` is pruned by retention, `file_ops` cascade-deletes, and `file_entries.pending_op_id` is set to NULL. If a `file_entry` was waiting on an op that just got pruned (e.g. the run completed >90 days ago but the entry never moved to `clean`), the entry silently loses its pointer and may be re-scheduled.
**Reasoning:**
- Premise: `RUN_HISTORY_RETENTION_DAYS = 90`. If a file entry is stuck in `pending_upload` for >90 days, retention would drop the sync_run referencing it.
- The `file_entry.sync_state` does not get reconciled by retention. After retention, the entry still says `pending_upload` but `pending_op_id IS NULL`.
- The engine reading this row will either (a) treat it as orphan and re-enqueue, or (b) ignore it. Neither is documented.

**Suggested direction:** Either (a) add a reconciliation pass in `retention::run` that flips any non-clean `file_entries` with NULL `pending_op_id` to `dirty`; or (b) refuse to prune a `sync_run` that has surviving `file_entries.pending_op_id` references.

### F14. `embed_migrations!` macro path is relative — works in dev, fragile under non-cargo workspaces — NIT
**Location:** `crates/onesync-state/src/migrations/mod.rs:9`
**Severity:** NIT
**Summary:** `embed_migrations!("src/migrations")` is a path relative to `CARGO_MANIFEST_DIR`. This works but is invisible to readers; if the file is moved (e.g. to `migrations/sql/`) the build break only surfaces at compile time.
**Suggested direction:** Add a doc-comment pointing at the directory; no code change needed.

### F15. No `panic!`/`todo!`/`unimplemented!` paths found in production code — informational (positive)
**Location:** all of `src/*.rs` (excluding `#[cfg(test)]` blocks)
**Severity:** Informational
**Summary:** Verified: every error path in `src/connection.rs`, `src/store.rs`, `src/retention.rs`, `src/queries/*.rs`, `src/migrations/mod.rs` propagates `Result`. The only `expect`/`unwrap` calls are inside `#[cfg(test)]` modules or `cfg(any(test, feature = "fakes"))` (the `fakes.rs` `.lock().expect(...)` pattern, which is conventional for `Mutex` poison and the file documents this). `connection.rs:60` uses `u32::try_from(STATE_POOL_SIZE).unwrap_or(4)` — `unwrap_or`, not panic.

## Cross-cutting observations

- **Per-connection PRAGMAs are set correctly** via `with_init` on the r2d2 manager — every borrow gets WAL, FK enforcement, busy timeout, `synchronous=NORMAL`, `temp_store=MEMORY`. This is the right pattern; a common bug is to set PRAGMAs once in `open()` rather than per-connection.
- **WAL + `synchronous=NORMAL` + pool of 4 + `busy_timeout=5s`** is a sensible default for a single-machine daemon. SQLite is single-writer, so 4 read connections + 1 implicit writer doesn't deadlock — but with `compact_now` taking an exclusive lock (see F4) the pool starves.
- **`schema.sql` faithfully reflects V001+V002+V003**: the trailing `, azure_ad_client_id ..., webhook_listener_port INTEGER, webhook_notification_url TEXT)` syntax for `instance_config` and `, webhook_enabled INTEGER NOT NULL DEFAULT 0)` for `pairs` are what `sqlite_schema.sql` produces after `ALTER TABLE ADD COLUMN`. The `xtask check-schema` compares trimmed text, so a fresh `cargo run -p xtask -- dump-schema` would catch a divergent migration. The script does *not* compare actual column types or check-constraints semantically — it's text-equality only, so renaming a column case-only (e.g. `payload_JSON`) would slip through.
- **Migrations are up-only, idempotent within a single connection** (refinery records applied versions in `refinery_schema_history`). No down-migrations exist; recovery from a half-applied migration is implicit (refinery wraps each migration in a tx in default mode — verify that the embed configuration doesn't set `runner().set_grouped(false)` somewhere; it doesn't, since `mod.rs` calls `.run(conn)` plain). So a SIGKILL mid-migration leaves the database with refinery's previous head; the next run will pick up where it left off.
- **`audit::search` dynamic SQL** is safe: every variable is bound via `?` and the only injected string-fragments are static clause segments (`" AND level = ?"`, `" AND pair_id = ?"`). Same for `pairs::list`. Confirming F8 is *not* a SQL-injection finding — purely a defensive nit.
- **Fakes parity is decent**: `InMemoryStore` enforces account-removal cascade (lines 208-246), unique-PK semantics by HashMap, and ordering rules (`sort_by_key`) that mirror the SQL. Known divergences are commented (silent no-op on `op_update_status` for missing rows). However the fake does *not* enforce:
  - The `pairs_local_path_uq` partial unique index (active pairs only). A test that creates two active pairs with the same local path passes against the fake and fails against SQLite — could mask engine bugs.
  - The `accounts_upn_uq` unique index.
- **No detected `unsafe`** in the crate (`#![forbid(unsafe_code)]` at lib.rs:5).

## What looks correct

- All write queries use `?N` parameter binding for values. Only the VACUUM INTO and the dynamic SQL clause-builders use string formatting, and the latter only for static clause fragments (F8 is a defensive nit, not a vulnerability).
- `instance_config` is correctly modelled as a singleton via `CHECK(id = 1)` and `INSERT OR IGNORE` in `ensure_present`.
- `pairs_local_path_uq` and `pairs_remote_uq` are partial unique indexes that correctly allow re-adding a previously-removed pair at the same path (the partial WHERE `status <> 'removed'` is right).
- `account_remove`'s reliance on FK cascade is correct *given* the V001 schema with `ON DELETE CASCADE` everywhere it matters.
- `update_status`'s `COALESCE(started_at, ?)` for `started_at` is the right pattern — we never want to overwrite the first-start timestamp.
- Refinery embedded migrations are configured by directory and validated by both unit tests (`migrations_apply_to_fresh_memory_db`, `migrations_are_idempotent_on_second_run`) and the `xtask check-schema` text-diff guard.
- Connection-level PRAGMAs are applied in `with_init` so every pooled connection gets them — not just the first.


