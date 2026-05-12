# 01 — Domain Model

**Status:** Draft · **Date:** 2026-05-11 · **Owner:** Stan

This page defines the canonical entities, identifier scheme, relationships, and lifecycle state
machines that the sync engine operates on. Every entity here has a corresponding `$def` in
[`canonical-types.schema.json`](canonical-types.schema.json); the schema is the authoritative
shape. Adapters and the IPC layer translate between this model and their respective wire formats.

---

## ID scheme

All identifiers are typed newtypes wrapping a string of the form `<prefix>_<ulid>` where the
ULID is the Crockford Base32 representation produced by `IdGenerator`. ULIDs sort
lexicographically by creation time and are 26 characters long; the full identifier is therefore
30 characters plus prefix.

| Prefix | Entity | Lifetime |
|---|---|---|
| `pair_` | `Pair` | Until removed via `onesync pair remove` |
| `acct_` | `Account` | Until sign-out or token revocation |
| `cfl_`  | `Conflict` | Until resolved or expired (`CONFLICT_RETENTION_DAYS`) |
| `run_`  | `SyncRun` | Until pruned (`RUN_HISTORY_RETENTION_DAYS`) |
| `op_`   | `FileOp` | Until terminal state, then pruned with its `SyncRun` |
| `aud_`  | `AuditEvent` | Until pruned (`AUDIT_RETENTION_DAYS`) |

ID generation is centralised in the `IdGenerator` port. Tests inject a deterministic generator
so fixture data is stable. IDs are never reused; deletion is logical for entities with foreign
key dependents (`Pair`, `Account`) and hard for the rest.

---

## Entities

### `Pair` (`pair_`)

A folder pair is the unit of synchronisation. Each pair binds one local directory to one
OneDrive item ID on one account. Pairs are independent: pausing or failing one pair does not
affect another.

- `id`: `PairId`
- `account_id`: `AccountId` — the OneDrive account that owns the remote folder
- `local_path`: absolute filesystem path, canonicalised
- `remote_item_id`: OneDrive `driveItem.id` of the remote folder root
- `remote_path`: human-readable path under the account drive, for display only
- `display_name`: short label (e.g. `Documents`)
- `status`: `PairStatus`
- `paused`: boolean — user-set pause
- `delta_token`: opaque OneDrive delta cursor; `None` until first successful delta call
- `created_at`, `updated_at`: timestamps from `Clock`
- `last_sync_at`: timestamp of the most recent successful `SyncRun`
- `conflict_count`: number of unresolved conflicts in `Conflict`

Constraints:

- `local_path` must be an existing, readable, writable directory at `Pair` creation time.
- `local_path` must not be a parent or child of another pair's `local_path` (no nested pairs).
- `remote_item_id` must resolve to a folder, not a file, at creation time.

### `Account` (`acct_`)

Represents a signed-in Microsoft identity. One account can back multiple pairs.

- `id`: `AccountId`
- `kind`: `AccountKind` — `Personal` or `Business`
- `upn`: user principal name returned by `/me` (`me@example.com`)
- `tenant_id`: Azure tenant GUID for Business accounts; `consumers` for Personal
- `drive_id`: the account's default drive id
- `display_name`: friendly name from `/me/displayName`
- `keychain_ref`: opaque handle into the macOS Keychain for the refresh-token entry
- `scopes`: list of OAuth scopes granted (a non-empty list)
- `created_at`, `updated_at`

The refresh token itself is never stored in the SQLite database; `keychain_ref` is the
identifier that the keychain adapter maps to the actual secret. See
[`06-state-store.md`](06-state-store.md) and [`04-onedrive-adapter.md`](04-onedrive-adapter.md).

### `FileEntry` (no public prefix, keyed by `(pair_id, relative_path)`)

The sync state record for a single file path within a pair. The `FileEntry` table is the
engine's source of truth for "what we have seen", against which both local and remote
observations are compared.

- `pair_id`: `PairId`
- `relative_path`: POSIX-style path relative to the pair root, NFC-normalised
- `kind`: `FileKind` — `File` or `Directory`
- `local`: `Option<FileSide>` — last observed local state, or absent
- `remote`: `Option<FileSide>` — last observed remote state, or absent
- `synced`: `Option<FileSide>` — the last state both sides agreed on
- `sync_state`: `FileSyncState`
- `pending_op`: `Option<FileOpId>` — the in-flight operation, if any
- `updated_at`

A `FileSide` is `{ kind, size_bytes, content_hash, mtime, etag, remote_item_id }`. `kind`
mirrors the `FileEntry.kind`. `etag` and `remote_item_id` are populated only on the remote
side; on the local side they are `None`. `content_hash` is BLAKE3 over the file content and
is `None` for directories. For files large enough to require streaming, BLAKE3 is computed
in `HASH_BLOCK_BYTES` increments.

### `FileOp` (`op_`)

A discrete unit of work the engine executes against an adapter. Each `FileOp` belongs to one
`SyncRun` and one `Pair`.

- `id`: `FileOpId`
- `run_id`: `SyncRunId`
- `pair_id`: `PairId`
- `relative_path`: target path within the pair
- `kind`: `FileOpKind` — `Upload`, `Download`, `LocalDelete`, `RemoteDelete`, `LocalMkdir`,
  `RemoteMkdir`, `LocalRename`, `RemoteRename` (conflict resolution decomposes into these
  primitives; there is no dedicated "resolve" op)
- `status`: `FileOpStatus`
- `attempts`: number of attempts (bounded by `RETRY_MAX_ATTEMPTS`)
- `last_error`: structured error envelope, or absent on success/pending
- `enqueued_at`, `started_at`, `finished_at`

### `Conflict` (`cfl_`)

A `Conflict` records a divergence between the local and remote sides that the policy chose to
preserve. Conflicts are not errors; they are normal, expected outcomes of concurrent edits.

- `id`: `ConflictId`
- `pair_id`: `PairId`
- `relative_path`: the contested path
- `winner`: `ConflictSide` — `Local` or `Remote`
- `loser_relative_path`: the new path the losing copy was renamed to
- `local_side`: snapshot of the local `FileSide` at detection time
- `remote_side`: snapshot of the remote `FileSide` at detection time
- `detected_at`: timestamp from `Clock`
- `resolved_at`: timestamp set when the conflict reaches a resolved state; `None` while
  pending
- `resolution`: `auto` when the keep-both policy completed without user input, `manual`
  when the user invoked `onesync conflicts resolve`; `None` while pending
- `note`: optional CLI-supplied note left during manual resolution

### `SyncRun` (`run_`)

A periodic or event-triggered pass over one pair. Runs are append-only history records.

- `id`: `SyncRunId`
- `pair_id`: `PairId`
- `trigger`: `RunTrigger` — `Scheduled`, `LocalEvent`, `RemoteWebhook`, `CliForce`,
  `BackoffRetry`
- `started_at`, `finished_at`
- `local_ops`, `remote_ops`: counts by kind
- `bytes_uploaded`, `bytes_downloaded`
- `outcome`: `RunOutcome` — `Success`, `PartialFailure(reason)`, `Aborted(reason)`

### `AuditEvent` (`aud_`)

Structured-log entry. Audit events are persisted in addition to being emitted to the daemon's
log stream so the CLI can replay history without re-tailing the log file.

- `id`: `AuditEventId`
- `ts`: timestamp from `Clock`
- `level`: `info` | `warn` | `error`
- `kind`: machine-readable event type
- `pair_id`: optional
- `payload`: opaque JSON object whose shape is keyed by `kind`

---

## Relationships

```
                                ┌─────────────┐
                                │   Account   │
                                └──────┬──────┘
                                       │ 1
                                       │
                                       │ N
                                ┌──────▼──────┐
                                │    Pair     │
                                └──┬──────────┘
                          ┌────────┼─────────────────────┐
                          │ 1      │ 1                   │ 1
                          │        │                     │
                          │ N      │ N                   │ N
                  ┌───────▼──┐ ┌───▼────┐         ┌──────▼──────┐
                  │ FileEntry│ │SyncRun │         │  Conflict   │
                  └──────────┘ └───┬────┘         └─────────────┘
                                   │ 1
                                   │
                                   │ N
                               ┌───▼─────┐
                               │ FileOp  │
                               └─────────┘
```

- `Account` → `Pair`: one-to-many. An account with zero pairs is allowed; the user may sign
  in before creating any pair.
- `Pair` → `FileEntry`: one-to-many. `FileEntry` is keyed by `(pair_id, relative_path)`.
- `Pair` → `SyncRun`: one-to-many, append-only.
- `SyncRun` → `FileOp`: one-to-many. A run with zero ops is valid (nothing changed).
- `Pair` → `Conflict`: one-to-many. A conflict's loser path becomes a new `FileEntry` after
  the rename completes.

---

## Lifecycle / state machines

### `Pair.status` — `PairStatus`

```
            register
   (none) ──────────► Initializing
                          │
                first delta complete
                          │
                          ▼
                       Active ────────────────────────┐
                          │                           │
                pause via CLI                         │ pair removed
                          │                           │ via CLI
                          ▼                           │
                       Paused                         │
                          │                           │
                resume via CLI                        │
                          │                           │
                          ▼                           │
                       Active                         │
                          │                           │
                fatal error                           │
                          │                           │
                          ▼                           │
                       Errored ──┐                    │
                          │      │ user resolves      │
                          │      │ (re-auth, fix path)│
                          │      ▼                    │
                          │   Active                  │
                          │                           │
                          └───────────────────────────┴──► Removed
```

States:

- `Initializing` — the pair has been registered but the first full delta has not yet completed.
  Sync ops are queued but not executed.
- `Active` — normal operation. Sync runs are scheduled and event-triggered.
- `Paused` — user-paused. No runs are scheduled. FSEvents continue to be observed but
  enqueued ops are not started.
- `Errored` — a non-recoverable error occurred (revoked auth, deleted remote folder,
  unmounted local volume). The pair stays in this state until the user takes corrective action.
- `Removed` — terminal. The row is marked deleted; the next compaction pass removes it
  permanently along with its `FileEntry`, `SyncRun`, and `Conflict` records.

### `FileEntry.sync_state` — `FileSyncState`

Per-file state, derived from comparing `local`, `remote`, and `synced` `FileSide`s on each
sync cycle.

```
                ┌─────────┐
                │  Clean  │  (local == remote == synced)
                └────┬────┘
                     │
       any change observed on either side
                     │
                     ▼
                ┌─────────┐
                │  Dirty  │
                └────┬────┘
                     │
        engine inspects sides:
                     │
   ┌─────────────────┼─────────────────────┐
   │ only local diff │ only remote diff    │  both diff
   │                 │                     │
   ▼                 ▼                     ▼
PendingUpload   PendingDownload       PendingConflict
   │                 │                     │
 op enqueued       op enqueued        rename + two ops
   │                 │                     │
   ▼                 ▼                     ▼
InFlight          InFlight              InFlight
   │                 │                     │
 success           success               success
   │                 │                     │
   ▼                 ▼                     ▼
                ┌─────────┐
                │  Clean  │
                └─────────┘
```

The transition from `Dirty` to `PendingConflict` triggers `Conflict` creation. The loser-rename
is itself a `FileOp` and produces a new `FileEntry` for the renamed path; both that entry and
the original then proceed independently.

### `FileOp.status` — `FileOpStatus`

```
   Enqueued ──► InProgress ──► Success
                    │
                    │ failure (retryable)
                    ▼
                Backoff ──► InProgress ─► …  (bounded by RETRY_MAX_ATTEMPTS)
                    │
                    │ exhausted or non-retryable
                    ▼
               Failed
```

`Failed` ops are not retried automatically. The owning pair transitions to `Errored` if the
failure category is non-recoverable; otherwise the pair remains `Active` and the user can
inspect the failure via CLI and trigger a re-run.

---

## Required query patterns

| Query | Used by | Access pattern |
|---|---|---|
| `pairs_for_account(account_id)` | CLI `pair list`, account removal | Indexed by `account_id` |
| `file_entries_dirty(pair_id)` | Sync engine cycle | Indexed by `(pair_id, sync_state)` |
| `file_entry_by_path(pair_id, relative_path)` | Adapter callbacks | Primary key |
| `ops_in_flight(pair_id)` | Sync engine concurrency cap | Indexed by `(pair_id, status)` |
| `conflicts_unresolved(pair_id)` | CLI `conflicts list` | Indexed by `(pair_id, resolved_at IS NULL)` |
| `recent_runs(pair_id, limit)` | CLI `status`, telemetry | Indexed by `(pair_id, started_at DESC)` |
| `audit_recent(limit)` | CLI `logs tail` | Indexed by `ts DESC` |

Schema-level indexes that back these queries are documented in
[`06-state-store.md`](06-state-store.md).

---

## Assumptions and open questions

**Assumptions**

- Path equality uses NFC normalisation. Two paths whose Unicode forms differ only by
  normalisation are treated as the same path. APFS preserves form on write but the engine
  must canonicalise before comparing or using as a key.
- BLAKE3 is the local content hash and the only hash recorded in `FileSide`. OneDrive's
  `sha1Hash` (Personal) and `quickXorHash` (Business) are verified streaming during downloads
  to guard against transport corruption; they are not persisted and are not part of
  `FileSide` equality.
- ULID timestamps are sufficient for ordering within a single instance. We do not depend on
  cross-host ULID ordering because there is only one daemon per user host.

**Decisions**

- *ULID identifiers with typed prefixes.* **`pair_<ulid>` style.** Sortable by creation time,
  compact, and the prefix prevents cross-entity ID confusion at the IPC and log boundary.
- *FileEntry keyed by `(pair_id, relative_path)`.* **No standalone file ID.** Paths change but
  rename detection is handled by hash-matching across deletes and creates inside one cycle;
  introducing a stable file ID would require a server-side identifier we cannot rely on for
  pre-existing local files.
- *Conflict produces a new FileEntry rather than mutating the original.* **The renamed loser is
  a first-class file from the engine's point of view.** It will sync up to OneDrive on the next
  cycle and become a normal `Clean` entry; the original path resumes its own lifecycle.
- *Append-only SyncRun history.* **Runs are never updated after they finish.** Pruning is
  governed by `RUN_HISTORY_RETENTION_DAYS`; mutation would lose audit value.
- *Symbolic link policy.* **Symlinks are skipped during scan and watch with a per-pair
  `local.symlink.skipped` audit event.** A future opt-in shallow-link mode is left for a later
  milestone; the schema is shaped so a `link_target` field can be added to `FileEntry` without
  a breaking migration. See [`05-local-adapter.md`](05-local-adapter.md) for the scanner-level
  detail.
- *Case-insensitive name collisions.* **OneDrive's stored name wins the canonical path; the
  local-side file that would otherwise collide on a case-insensitive comparison is renamed with
  a `(case-collision-<short-hash>).ext` suffix and synced up as a new file.** This applies on
  case-sensitive APFS volumes where the local side can legitimately hold both `Report.pdf` and
  `report.pdf`; on case-insensitive volumes (the default) the collision cannot occur. The
  renamed loser flows through the same pipeline as any other rename — it appears in
  `onesync conflicts list` for review, then becomes a normal `Clean` entry on the next cycle.
  This decision subsumes the case-sensitive APFS open question previously tracked in
  [`05-local-adapter.md`](05-local-adapter.md).

**Open questions**

- *Opt-in shallow symlink sync.* The current skip-with-warning policy is final for MVP; an
  opt-in mode that records `link_target` and syncs it as a string payload remains a possible
  future feature. No work is committed.
