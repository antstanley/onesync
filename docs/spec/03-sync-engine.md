# 03 — Sync Engine

**Status:** Draft · **Date:** 2026-05-11 · **Owner:** Stan

The sync engine is the heart of onesync. It owns the loop that detects changes on both sides,
reconciles them against the last-known synced state, applies the conflict policy, and produces
a bounded set of `FileOp`s to drive into the adapters. This page defines the cycle, the
detection mechanisms, the reconciliation rules, the conflict policy, the retry behaviour, and
the back-pressure model.

The engine lives in `onesync-core::engine`. It has no I/O of its own; all observation and
action passes through ports.

---

## Responsibilities

1. Schedule and run one sync cycle per pair, triggered by either a debounced local event, a
   remote webhook (or fallback poll), the operator (`onesync pair sync --now`), or a
   scheduled interval.
2. Maintain the `FileEntry` index for each pair as the projection of the last observed local
   and remote `FileSide`s plus the last `synced` `FileSide`.
3. Decide, for every observed difference, which side's value becomes canonical, whether a
   conflict is needed, and which `FileOp`s to enqueue.
4. Apply the [keep-both conflict policy](#conflict-policy) when both sides have diverged from
   `synced`.
5. Retry transient adapter failures with exponential backoff plus jitter, bounded by
   `RETRY_MAX_ATTEMPTS`.
6. Pause a pair (set `PairStatus::Errored`) on non-recoverable errors and audit the event.
7. Honour back-pressure: never enqueue more than `MAX_QUEUE_DEPTH_PER_PAIR` ops per pair,
   never run more than `MAX_CONCURRENT_TRANSFERS` transfers across all pairs.

---

## Cycle structure

A cycle for one pair has six phases. Each phase is bounded; if any phase exceeds
`CYCLE_PHASE_TIMEOUT_MS`, the cycle is aborted with a `PartialFailure` outcome and the pair is
re-scheduled after backoff.

```
┌──────────────────────────────────────────────────────────┐
│ 0  Acquire pair lock (single-cycle-per-pair invariant)   │
│ 1  Collect events: drained debounced local + remote      │
│ 2  Local scan delta:  walk affected paths, hash, compare │
│ 3  Remote scan delta: paged /me/drive/root:/.../delta    │
│ 4  Reconcile:         join local Δ + remote Δ + FileEntry│
│ 5  Plan FileOps:      conflict policy + per-path actions │
│ 6  Execute FileOps:   bounded concurrency, persist state │
│ 7  Record SyncRun:    append-only history entry          │
└──────────────────────────────────────────────────────────┘
```

The lock in phase 0 is an `async` mutex on the pair record held by the pair's owning task.
Cross-task IPC commands (force-sync, pause, remove) coordinate by sending on the pair's
`mpsc` channel, never by taking the lock from outside the owning task.

---

## Triggers and scheduling

The pair's owning task wakes up on any of these signals:

| Trigger | Source | Debounce | Notes |
|---|---|---|---|
| `LocalEvent` | FSEvents stream from `LocalFs::watch` | `LOCAL_DEBOUNCE_MS` | Multiple FSEvents within the window collapse into one cycle. |
| `RemoteWebhook` | `/subscriptions` callback (when registered) | `REMOTE_DEBOUNCE_MS` | Webhooks are sketched but not enabled by default; see [04](04-onedrive-adapter.md). |
| `Scheduled` | `Clock::now` tick | n/a | Fixed period of `DELTA_POLL_INTERVAL_MS`, doubled under throttling. |
| `CliForce` | `pair_force_sync` RPC | n/a | Bypasses debounce; respects all other limits. |
| `BackoffRetry` | Engine itself, after a failed cycle | exponential | `RETRY_BACKOFF_BASE_MS` * 2^attempts, with full jitter. |

Phase 1 drains all signals collected during the previous cycle plus the debounce window.
A pair never has more than one cycle in flight; subsequent triggers coalesce.

---

## Change detection

### Local

`LocalFs::watch` yields a stream of `LocalEvent { kind: Created | Modified | Deleted | Renamed,
path: AbsPath }`. The engine maintains a per-pair `local_dirty: BTreeSet<RelPath>`; events
are converted to relative paths and inserted. Renames produce two entries (old + new).

At cycle time, the engine drains the set and, for each path, asks `LocalFs` for the current
metadata (`size`, `mtime`, `kind`). If the file disappeared since the event, the engine
records a tentative `LocalSide = None`. Hashes are computed lazily during reconciliation,
not in this phase.

`LocalEvent` losses (overrun of FSEvents' kernel queue) are detected by the watcher's
`overflow` flag; when set, the next cycle promotes to a full-scan reconcile for that pair
before resuming event-driven mode.

### Remote

`RemoteDrive::delta` returns a page of `DriveItem` deltas plus an opaque cursor. The engine
stores the cursor in `Pair.delta_token`. If the cursor is `None` (first call ever or invalidated
by the server), the call returns a full inventory; the engine processes it as the initial
import.

Page handling:

- The cursor is advanced **only after** the engine has persisted every item in the page into
  `FileEntry.remote`. Crashes mid-page cause the page to be re-emitted on next call; the
  reconcile is idempotent.
- When the server signals invalidation (`resyncRequired`), the engine drops the cursor,
  marks the pair as `Initializing`, and runs a full re-sync.

### Initial sync

The first cycle after a pair is registered is the **initial sync**. The engine:

1. Lists the local folder via `LocalFs::scan`.
2. Calls `RemoteDrive::delta` with no cursor.
3. For each path that exists on exactly one side, enqueues the matching `Upload` or `Download`.
4. For each path that exists on both sides with matching size and hash, marks `Clean`
   without transferring.
5. For each path that exists on both sides with differing content, treats this as a
   first-time-seen `Dirty` and applies the standard policy (newer mtime wins canonical;
   loser renamed).

Initial sync is the only time the engine compares pre-existing files. After the first cycle,
`FileEntry.synced` is always populated and `Dirty` transitions are driven by deltas, not by
ambient comparison.

---

## Reconciliation

For each path observed in either delta during a cycle, the engine reads `FileEntry` and joins:

```
  synced  ── what we last agreed on
  local   ── what local now looks like (may be unchanged)
  remote  ── what remote now looks like (may be unchanged)
```

The decision table is small:

| `synced` vs `local` | `synced` vs `remote` | Action |
|---|---|---|
| equal | equal | mark `Clean` (no-op) |
| differs | equal | enqueue `Upload` if file, `RemoteMkdir`/`RemoteRename`/`RemoteDelete` as appropriate |
| equal | differs | enqueue `Download`, `LocalMkdir`, `LocalRename`, `LocalDelete` |
| differs | differs (and `local` ≠ `remote`) | apply **conflict policy** |
| differs | differs (and `local` == `remote`) | both sides converged independently; mark `Clean` with new `synced = local` |

Equality is defined as `(kind, size_bytes, content_hash)` matching. `mtime` is **not** part of
equality; it is used only for tie-breaking in conflict resolution.

`synced == None` (file never seen) means the row is being created this cycle; treat the
empty `synced` as "differs from both sides" — i.e. the row joins the conflict path if both
sides have it with different content.

---

## Conflict policy

When the engine determines that both sides have diverged from `synced` and from each other,
the keep-both policy runs.

### Winner selection

The side with the higher `mtime` wins the canonical path. Ties (within
`CONFLICT_MTIME_TOLERANCE_MS`, default 1000 ms) resolve in favour of `Remote`, on the
principle that OneDrive's central server is a more reliable mtime source than a laptop with
a possibly-skewed clock.

Tie-break rationale is logged on every conflict; users do not have to infer.

### Loser rename

The loser is renamed (on its own side) before either side's content is overwritten.

Rename target:

```
<stem> (conflict <YYYY-MM-DDTHH-MM-SSZ> from <host>).<ext>
```

Where:

- `<stem>` and `<ext>` are derived from the original `relative_path`. Files with no
  extension produce `<stem> (conflict … from <host>)`.
- `<YYYY-MM-DDTHH-MM-SSZ>` is the engine's UTC timestamp at conflict detection.
- `<host>` is the local machine's `gethostname()` for `Local` losers; for `Remote` losers it
  is the `lastModifiedBy.user.displayName` if available, else `remote`.

If the renamed name would itself collide (extreme edge), the engine appends `-2`, `-3`, etc.,
bounded by `CONFLICT_RENAME_RETRIES`. Exhaustion is logged and the conflict surfaces as
unresolved.

### Op sequence

The conflict resolution produces an ordered op group, committed atomically to `StateStore`
before any side-effect:

1. **Rename loser** on its own side first. Until this succeeds the engine does not touch the
   canonical path.
2. **Propagate the rename** to the other side as a normal upload/download (a brand-new file
   from the other side's perspective).
3. **Propagate the winner's content** to the loser's side, overwriting the now-vacant
   canonical name.
4. **Insert a `Conflict` row** with both `FileSide` snapshots and the chosen winner.

A conflict is considered `Auto`-resolved when steps 1–4 all succeed without operator input.
The CLI's `conflicts list` shows `Auto` and `Pending` conflicts separately so users can audit
without having to act.

### Operator override

`onesync conflicts resolve <id> --pick local|remote --keep-loser|--discard-loser` lets the user
post-hoc declare a different winner. The engine then performs the inverse transfer; the
loser-rename is kept by default but can be discarded explicitly. The override never operates
across sides without a fresh hash check to confirm nothing else changed in the meantime.

---

## Op execution

`FileOp`s are pulled by a per-pair executor with concurrency bounded by
`PAIR_CONCURRENT_TRANSFERS` (default 2) and a global ceiling of `MAX_CONCURRENT_TRANSFERS`
(default 4) enforced by a shared semaphore.

Each op:

1. Transitions to `InProgress`.
2. Calls the relevant adapter method.
3. On success: updates `FileEntry.synced` to match the post-op state and transitions to
   `Success`.
4. On retryable failure: transitions to `Backoff`, increments `attempts`, schedules re-execution
   after `RETRY_BACKOFF_BASE_MS * 2^attempts` with full jitter, up to `RETRY_MAX_ATTEMPTS`.
5. On non-retryable failure or after retry exhaustion: transitions to `Failed`, audits, and
   leaves the `FileEntry` `Dirty` for re-attempt on the next cycle.

Retryable categories:

- Network errors with no response (timeout, DNS failure, connection reset).
- HTTP 408, 429, 500, 502, 503, 504.
- HTTP 401 with a `Bearer` challenge — counts as one attempt, triggers token refresh.

Non-retryable categories:

- HTTP 4xx other than the above. Surfaced through the audit log and CLI status.
- Local `PermissionDenied`, `NotADirectory`, `InvalidInput`.
- `FileTooLarge` (over `MAX_FILE_SIZE_BYTES`).

Op order within a single phase respects two invariants: **directories before their files**
(create parents first), and **deletes after creates** (so a same-cycle rename modelled as
delete+create never loses data even if the actor dies between steps).

---

## Throttling and back-pressure

The engine cooperates with two back-pressure signals.

### Adapter signals

`GraphError::Throttled { retry_after }` is the most common. The engine treats it as a
pair-wide pause: every in-flight op for the pair is moved to `Backoff` with the
`retry_after` value, and the next cycle is rescheduled past that deadline. `Retry-After` from
the Graph API is the source of truth.

`LocalFsError::DiskFull` and `LocalFsError::QuotaExceeded` pause the pair entirely until a
free-space check (`DISK_FREE_MARGIN_BYTES`) clears.

### Engine-internal limits

`MAX_QUEUE_DEPTH_PER_PAIR` is the upper bound on enqueued ops per pair. When the engine
would exceed it during planning, it stops planning new ops, records `Planning::Truncated` in
the run, and finishes the cycle. The next cycle picks up the remaining work.

`MAX_CONCURRENT_TRANSFERS` is the global semaphore; pairs share it fairly via a round-robin
within the executor.

---

## Failure modes

| Symptom | Detection | Engine response |
|---|---|---|
| FSEvents queue overflow | `overflow` flag on `LocalEventStream` | Promote next cycle to full local scan; log `local.fsevents.overflow`. |
| Delta token invalidated | `GraphError::ResyncRequired` | Drop `Pair.delta_token`; next cycle is a full remote re-scan. |
| Token revoked | `GraphError::Unauthorized` after refresh | Set `Pair.status = Errored("auth")`; surface to CLI. |
| Local volume unmounted | `LocalFsError::NotMounted` | Set `Pair.status = Errored("local-missing")`; pause; re-check on volume mount. |
| Remote folder deleted | `GraphError::NotFound` on root probe | Set `Pair.status = Errored("remote-missing")`; require operator action. |
| Clock skew > `MAX_CLOCK_SKEW_S` | `RemoteItem.lastModified` future-dated by > limit | Log `clock.skew`; conflict tie-break ignores both clocks and falls through to `Remote`. |

Every failure mode emits a structured `AuditEvent` whose `kind` is the canonical name listed
in the table.

---

## Observability

Every cycle emits:

- `cycle.start` with `pair_id`, `trigger`, `started_at`.
- `phase.timing` per phase, with elapsed milliseconds.
- `op.enqueued`, `op.started`, `op.finished` per `FileOp`.
- `conflict.detected`, `conflict.resolved.auto`, `conflict.resolved.manual`.
- `cycle.finish` with `outcome`, counts, bytes.

These are persisted as `AuditEvent`s and streamed to the daemon's structured log.

---

## Assumptions and open questions

**Assumptions**

- `Clock::now` is monotonic enough for tie-breaks within a cycle. The engine does not assume
  monotonicity across cycles; it always re-reads.
- Hash collisions on BLAKE3 are not a concern in equality checks. The cost of a 256-bit
  collision is effectively zero in this context.
- FSEvents debouncing at `LOCAL_DEBOUNCE_MS` is adequate. macOS file editors that write
  through atomic rename produce a brief burst we want to coalesce, not amplify.

**Decisions**

- *Newer-mtime wins canonical name, remote wins on ties.* **Tie-break in favour of remote.**
  Server clocks are more reliable; operator can override via `conflicts resolve`.
- *Conflict produces a rename then a normal sync.* **The renamed loser flows through the
  normal pipeline.** This avoids a second policy path inside the engine; the renamed file is
  just a new file from the system's point of view.
- *Per-pair owning task with mpsc.* **All cross-task communication to a pair goes through
  its channel.** No shared mutable state; the executor's bounded concurrency is the only
  serialisation point.
- *Equality is `(kind, size, hash)`; mtime is metadata.* **mtime is never load-bearing for
  equality.** Some editors zero mtime, some preserve it across copies; only content equality
  is reliable.

**Open questions**

- *Webhook-driven remote change detection.* MSGraph supports `/subscriptions` against
  `driveItem`. The engine currently polls; the webhook path is sketched but the receiver URL
  story (local tunnel? skip on locked-down networks?) is unresolved.
- *Adaptive `DELTA_POLL_INTERVAL_MS`.* Current behaviour doubles under throttling; we have
  no data yet on whether to also tighten the interval when local change rate is high.
