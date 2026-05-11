# 05 — Local Filesystem Adapter

**Status:** Draft · **Date:** 2026-05-11 · **Owner:** Stan

The local adapter implements the `LocalFs` port against the macOS filesystem. It owns
FSEvents-driven change detection, content hashing, atomic file writes, and the small set of
metadata reads the engine needs. It also enforces filesystem-shape invariants (path
normalisation, symlink policy, attribute handling) so that the engine sees a clean,
predictable view.

The crate is `onesync-fs-local`. It depends on `notify` (FSEvents on macOS), `blake3`, and
`fs2` for advisory locks. Atomic writes use `tempfile` + `rename(2)` on the same volume.

---

## Responsibilities

1. Watch a pair's local root via FSEvents and produce a debounced, normalised
   `LocalEventStream`.
2. Scan a root recursively into a stream of `LocalFileMeta { path, kind, size, mtime }`.
3. Hash a file's contents with BLAKE3, in `HASH_BLOCK_BYTES` chunks, returning a `ContentHash`.
4. Read a file as a streaming source the engine can pipe into uploads.
5. Write a file atomically: temp file in the same directory, fsync, rename, fsync directory.
6. Rename, delete, and `mkdir -p` paths.
7. Enforce path normalisation (NFC), reject paths outside the pair root, and skip symlinks
   per policy.
8. Detect volume unmounts and surface them as `LocalFsError::NotMounted`.

---

## Path discipline

All paths handed to the engine are **relative** to the pair root, in **POSIX form** with `/`
separators, **NFC-normalised**, with no leading `/`, no `..`, and no embedded NUL. Two paths
that differ only by Unicode normalisation map to the same `RelPath`; APFS stores filenames in
the form provided on write and preserves it on read, so we normalise on the way in and
preserve on the way out by remembering the original form per path.

Validation happens at the adapter boundary:

- `LocalEventStream` filters events whose paths cannot be made relative to the watched root.
- `scan` skips entries whose names fail UTF-8 decoding (NFKC-incompatible bytes from external
  filesystems mounted under macOS); a warning is emitted and the path is recorded as skipped
  in the cycle's audit.
- `write_atomic` rejects target paths containing `..` or absolute components.

Cross-volume operations are detected and degraded: a `rename` across volumes degrades to
copy-then-delete, with each step fsync'd. The adapter exposes the degradation in
`LocalFsError::CrossVolumeRename(method = "copy+delete")` for audit visibility.

---

## FSEvents watcher

The watcher is constructed once per pair against the pair's local root. We use `notify`'s
`RecommendedWatcher`, which on macOS is `FsEventWatcher` (a wrapper over CoreServices'
`FSEventStream`). Configuration:

- `RecursiveMode::Recursive` so child paths produce events.
- Latency: `LOCAL_DEBOUNCE_MS` (default 500 ms) collapses bursts.
- The `kFSEventStreamCreateFlagNoDefer` flag is enabled to flush events quickly when the
  daemon wakes from idle.
- Event coalescing flag `kFSEventStreamCreateFlagFileEvents` is enabled so file-granularity
  events (not just directory-granularity) are reported.

Event translation:

| FSEvents flag | Adapter `LocalEvent.kind` |
|---|---|
| `kFSEventStreamEventFlagItemCreated` | `Created` |
| `kFSEventStreamEventFlagItemModified` | `Modified` |
| `kFSEventStreamEventFlagItemRemoved` | `Deleted` |
| `kFSEventStreamEventFlagItemRenamed` | `Renamed` (paired with subsequent event for new name) |
| `kFSEventStreamEventFlagMustScanSubDirs` | `Overflow` (flag, not event) |
| `kFSEventStreamEventFlagUnmount` | adapter emits `NotMounted` and ends the stream |

The watcher emits a single `Overflow` flag on the `LocalEventStream` when the kernel buffer
overflowed. The engine treats this as a signal to run a full-scan reconcile next cycle.

The watcher never blocks the runtime: the underlying FSEvents callback runs on a dedicated
CoreFoundation runloop thread and pushes into a bounded `tokio::sync::mpsc` channel of size
`FSEVENT_BUFFER_DEPTH` (default 4096). When the channel is full, the adapter sets the
`Overflow` flag and drops the event (the engine will re-scan).

---

## Scanning

`scan(root)` returns a `LocalScanStream` of `LocalFileMeta`. The scan:

- Walks the directory tree breadth-first, async, with an explicit `VecDeque` worklist
  bounded by `SCAN_QUEUE_DEPTH_MAX`. Per-directory enumeration uses `tokio::fs::read_dir`;
  there is no recursion in our code.
- Records `size`, `mtime`, and `kind` (`File` / `Directory`) from each `Metadata` call.
- **Does not** hash during scan. Hashing happens in reconciliation when an actual content
  comparison is needed.
- Skips symlinks (records them in the audit as `local.symlink.skipped`).
- Skips `.DS_Store`, `Icon\r`, `.localized`, and macOS resource fork sidecars (`._*`); the
  full skip-list lives in `SKIPPED_LOCAL_NAMES`.
- Skips files whose absolute path length exceeds `MAX_PATH_BYTES` (1024) with a warning.

The scan stream is bounded so very large folders do not blow memory: the producer fills up to
`SCAN_INFLIGHT_MAX` entries and back-pressures.

---

## Hashing

`hash(path)` opens the file, streams it through a BLAKE3 hasher in `HASH_BLOCK_BYTES`
(default 1 MiB) chunks, and returns a 32-byte digest plus the observed `size` and `mtime`
snapshot taken **after** the final read. The post-hash snapshot is critical: if `mtime`
changed during hashing we declare a transient `LocalFsError::Raced` and the engine retries.

Hashing runs on `spawn_blocking` to keep the runtime responsive. The hasher is allocated once
per call; we do not pool hashers.

The cross-check hashes used for download verification (SHA-1 for Personal, QuickXorHash for
Business) are also implemented in this crate but are evaluated lazily, only when we actually
need to verify a download.

---

## Atomic writes

`write_atomic(path, stream)` is the only way the engine writes a file. Algorithm:

```
1. Resolve `path` relative to the pair root; verify normal form.
2. Create a temp file `<dir>/.onesync-tmp-<ulid>` in the target directory (same volume).
3. Stream input bytes to the temp file, hashing as we go.
4. fsync the temp file fd.
5. Rename the temp file over `path`. (rename(2) is atomic within a volume.)
6. fsync the directory fd to durably record the rename.
7. Return the `FileSide { content_hash, size_bytes, mtime, etag: None }`.
```

The temp file is created with the same `O_CLOEXEC` flag the daemon uses elsewhere; on cleanup
of partial writes, leftover temp files are swept on daemon start (`startup.gc.tempfiles`).

`write_atomic` does **not** preserve macOS metadata beyond mtime. Extended attributes,
resource forks, ACLs, and Finder tags are out of scope. The audit emits
`local.metadata.dropped` when a write replaces a file that previously had extended
attributes, so users can verify expectations.

---

## Renames and deletes

`rename(from, to)` uses `rename(2)` when on the same volume. Cross-volume renames degrade
(see [Path discipline](#path-discipline)).

`delete(path)` uses `unlink(2)` for files, `rmdir(2)` for empty directories. Non-empty
directories are not deleted by the adapter; the engine planner emits per-file deletes first.
This avoids any risk that an unexpected file is destroyed in a recursive removal.

The adapter never moves files to the macOS Trash. Trash semantics are not a fit for an
automated sync: the OneDrive Recycle Bin is the authoritative recoverable-delete location.

---

## Locks

The daemon takes an exclusive advisory lock on `<state-dir>/onesync.lock` via `fs2` at
startup. A second daemon instance fails with `LocalFsError::AlreadyRunning`. The CLI's
`onesync status` reports the holder's PID by reading the daemon's PID file alongside the
lock.

Per-pair locks are not advisory filesystem locks — they are in-memory async mutexes inside
the daemon process (see [02 — Architecture](02-architecture.md#concurrency-model)) and do
not serialise against other processes touching the same files. We assume the user does not
run a second sync client over the same folder; if they do, the conflict policy handles the
divergence harmlessly.

---

## Volume mount handling

The adapter watches the FSEvents stream's unmount notifications. When the pair's root
volume disappears:

- The `LocalEventStream` emits `LocalEvent { kind: Unmounted, .. }` and closes.
- Subsequent calls to `read`/`write_atomic`/`rename`/`delete` return
  `LocalFsError::NotMounted`.
- The engine transitions the pair to `Errored("local-missing")`.

The adapter also polls (via `DiskArbitration` notifications or a 30-second timer fallback)
for the volume re-appearing; on remount it surfaces `LocalEvent::Remounted` so the engine
can re-initialise the watcher and run a full-scan reconcile.

---

## Hidden-file and permissions policy

- Files whose names start with `.` are synced **only** if they are not in
  `SKIPPED_LOCAL_NAMES` and they are under the pair root. Skipping dotfiles by default would
  be surprising to users who keep configuration files in their OneDrive folder.
- Files without the user's read permission produce `LocalFsError::PermissionDenied` on
  upload. The op fails non-retryably and surfaces; the user is asked to fix permissions.
- The daemon never escalates privileges. There is no `sudo` or `xattr` fallback.

---

## Error mapping

`LocalFsError` is the port-level error. The notable variants:

| Cause | Variant | Engine treatment |
|---|---|---|
| `ENOENT` on read | `NotFound` | Treated as `LocalSide = None` |
| `EACCES` / `EPERM` | `PermissionDenied` | Non-retryable; op `Failed`, audit |
| `EROFS` / unmount | `NotMounted` | Pair → `Errored("local-missing")` |
| `ENOSPC` | `DiskFull` | Pair paused until `DISK_FREE_MARGIN_BYTES` available |
| `EDQUOT` | `QuotaExceeded` | Same as `DiskFull` |
| `EEXIST` on `mkdir` | `Exists` | Treated as success if the existing item is a directory |
| `EXDEV` on `rename` | `CrossVolumeRename` | Degrades to copy+delete |
| FSEvents overflow | `Overflow` | Flag on the stream; engine triggers full-scan reconcile |
| `Raced` (mtime changed during hash) | `Raced` | Standard retry |
| Path validation failed | `InvalidPath { reason }` | Non-retryable; surfaced |

---

## Assumptions and open questions

**Assumptions**

- The pair root is on a local APFS volume. Network filesystems (SMB, NFS) and `FUSE` volumes
  are not supported; FSEvents on those is unreliable.
- `mtime` is preserved across atomic rename. APFS preserves mtime on rename within a volume.
- BLAKE3 throughput on modern Apple Silicon is well above NVMe read throughput; hashing is
  not the bottleneck.

**Decisions**

- *Atomic write via temp-rename plus dir-fsync.* **Standard POSIX recipe.** Crash safety in
  exchange for one extra fsync per directory write; acceptable.
- *No extended-attribute sync.* **Drop xattrs on writes; warn in audit.** Cross-OS sync of
  macOS-specific metadata is a rabbit hole; this surface is intentionally out of scope.
- *Hashing on `spawn_blocking`.* **Keeps the Tokio reactor responsive on large files.**
- *No Trash on delete.* **`unlink(2)` directly; OneDrive Recycle Bin is the recovery path.**
  Trash + xattr-based original-location tracking is too platform-specific for a sync engine.

**Open questions**

- *Case-sensitive APFS volumes.* APFS supports a case-sensitive variant; if a user formats
  their volume that way, two paths differing only in case become distinct. The engine
  currently does not handle this case; ticket pending.
- *Spotlight indexing interference.* `mds` can hold files open and create `mdimporter`
  events that the watcher sees as `Modified`; we expect debouncing to absorb most of these,
  but unconfirmed in long-soak tests.
- *FUSE volumes (rclone, mountainduck).* Currently unsupported; whether to detect and
  refuse versus behave imperfectly with a warning is open.
