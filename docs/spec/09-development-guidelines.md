# 09 — Development Guidelines (onesync)

**Status:** Draft · **Date:** 2026-05-11 · **Owner:** Stan

> **Read first:** the repository's cross-project development guidelines at
> <https://gist.github.com/antstanley/5bdaa85e63427fadae1c58ae6db77c27>. That document defines
> Tiger Style, the toolchain, defensive-coding rules, Rust conventions, version-control
> rules, the testing pyramid, repository hygiene, AI-agent rules, and the definition of
> done. This page only records the onesync-specific deltas and concrete values that those
> meta-rules require each project to declare.

onesync follows the repo-wide guidelines without exception unless this page explicitly
overrides. Where the gist says "the project declares a value", that value lives below.

---

## Project-specific deltas

### Language scope

- **Rust only.** The repository contains no TypeScript or Svelte code. The TypeScript and
  Svelte sections of the gist do not apply.
- **`#![forbid(unsafe_code)]`** is the crate-level default. The only crate that opts out is
  `onesync-fs-local` (notify wraps FSEvents internally; we still forbid `unsafe` in our
  own code) and `onesync-keychain` (wraps `security-framework`, which itself uses
  `unsafe` internally; our code never adds `unsafe` blocks). Any opt-out crate adds a
  `// SAFETY:` comment over every `unsafe` block it owns and is review-mandatory.
- **No `std::env::var` outside `onesync-daemon`'s startup module.** Configuration goes
  through the `InstanceConfig` row; environment is for path overrides only.

### Toolchain pins

- Rust 1.95.0 (rustup `rust-toolchain.toml` pinned).
- Targets: `aarch64-apple-darwin` and `x86_64-apple-darwin`. CI builds both.
- `cargo deny` clean (advisory DB up to date, licenses on the allow list).
- `cargo nextest` for tests. `cargo test` is not used.
- `cargo llvm-cov` for coverage; floor 50%, target higher per crate.

### Testing

- **Unit tests** live next to the code in `#[cfg(test)]` modules.
- **Integration tests** live under `crates/<adapter>/tests/` against fakes for adjacent
  ports and the real adapter for the system under test (e.g. SQLite for `onesync-state`,
  `tempfile` directories for `onesync-fs-local`).
- **End-to-end tests** live under `tests/e2e/`. They drive the daemon binary, talk over the
  IPC socket, hit a real Microsoft Graph endpoint against a dedicated test tenant, and run
  in the `slow` test tier. Local laptops do not run them by default; CI runs them on PRs to
  `main`.
- **Property tests** (`proptest`) for: path normalisation, conflict naming, retry backoff
  bounds, delta cursor monotonicity, file-entry state machine.
- **Mandatory negative-space tests** for: every limit (1-below, at, 1-above), every
  non-retryable error category, every malformed-input path on the IPC.
- **Deterministic time and IDs** in tests. `Clock` and `IdGenerator` ports inject fakes.
- **Test data** built from small builders, not megabyte fixtures.

### Assertions

- ≥ 2 meaningful assertions per function in `onesync-core`. The threshold is enforced by a
  `cargo xtask audit-asserts` lint that fails the build.
- `assert!`, `debug_assert!`, `assert_eq!` are encouraged. `assert!(true, "…")` placeholders
  are forbidden.
- `unwrap()`, `expect()`, and `panic!()` are forbidden outside `#[cfg(test)]` code and
  outside crate-root `main()` early-exit paths that fail with `EX_CONFIG`.
- `Result`'s `Ok` branch carries the type; the `?` operator is encouraged. `Err` branches
  must be matched exhaustively where the engine needs to differentiate retryable vs not.

### Errors

- One `Error` enum per crate, via `thiserror`. Variants name the failure category.
- Port-level errors live in `onesync-core` next to the trait that returns them.
- Errors crossing the IPC are serialised through `onesync-protocol`'s `RpcError`; adapter
  internals never leak past the daemon.
- No `Box<dyn Error>` in production paths. Test helpers may use `anyhow` to simplify
  fixture wiring.

### IDs and time

- IDs from `IdGenerator`. Never `Ulid::new()` directly outside that adapter.
- Time from `Clock`. Never `chrono::Utc::now()` or `std::time::SystemTime::now()` in core
  logic.
- Stored timestamps are ISO-8601 UTC strings in the database, `Timestamp` newtype in code.

---

## onesync-specific named limits

Every limit is a `pub const` in `onesync_core::limits` with units in the name. This is the
complete list at the time of writing; new limits must be added here and to the const module
in the same change.

### Sync engine

| Constant | Value | Units | Notes |
|---|---|---|---|
| `MAX_PAIRS_PER_INSTANCE` | 16 | pairs | Upper bound on concurrent folder pairs. |
| `MAX_QUEUE_DEPTH_PER_PAIR` | 4096 | ops | Planning truncates when reached. |
| `MAX_CONCURRENT_TRANSFERS` | 4 | global | Across all pairs. |
| `PAIR_CONCURRENT_TRANSFERS` | 2 | per pair | Internal scheduler share. |
| `RETRY_MAX_ATTEMPTS` | 5 | attempts | Per `FileOp`. |
| `RETRY_BACKOFF_BASE_MS` | 1_000 | ms | Exponential with full jitter. |
| `DELTA_POLL_INTERVAL_MS` | 30_000 | ms | Doubled under throttling, capped at 5 min. |
| `LOCAL_DEBOUNCE_MS` | 500 | ms | FSEvents coalescing window. |
| `REMOTE_DEBOUNCE_MS` | 2_000 | ms | Webhook coalescing window. |
| `CYCLE_PHASE_TIMEOUT_MS` | 60_000 | ms | Hard timeout per cycle phase. |
| `CONFLICT_MTIME_TOLERANCE_MS` | 1_000 | ms | Tie-break window. |
| `CONFLICT_RENAME_RETRIES` | 8 | attempts | Loser-name disambiguation cap. |

### Filesystem

| Constant | Value | Units | Notes |
|---|---|---|---|
| `MAX_FILE_SIZE_BYTES` | 50 * GiB | bytes | Hard cap on a single file. |
| `MAX_PATH_BYTES` | 1024 | bytes | UTF-8 absolute path length cap. |
| `HASH_BLOCK_BYTES` | 1 * MiB | bytes | BLAKE3 streaming block size. |
| `READ_INLINE_MAX` | 64 * KiB | bytes | Below this, reads stay on the Tokio reactor. |
| `FSEVENT_BUFFER_DEPTH` | 4096 | events | Bounded mpsc from watcher thread. |
| `SCAN_QUEUE_DEPTH_MAX` | 65_536 | dir entries | Bounded BFS during initial scan. |
| `SCAN_INFLIGHT_MAX` | 1024 | files | Backpressure on scan stream. |
| `DISK_FREE_MARGIN_BYTES` | 2 * GiB | bytes | Below this, downloads pause. |

### Microsoft Graph

| Constant | Value | Units | Notes |
|---|---|---|---|
| `GRAPH_SMALL_UPLOAD_MAX_BYTES` | 4 * MiB | bytes | Boundary between single-PUT and session. |
| `SESSION_CHUNK_BYTES` | 10 * MiB | bytes | Multiple of 320 KiB; Graph requirement. |
| `GRAPH_RPS_PER_ACCOUNT` | 8 | requests/s | Token bucket. |
| `TOKEN_REFRESH_LEEWAY_S` | 120 | seconds | Refresh if expiring within this. |
| `AUTH_LISTENER_TIMEOUT_S` | 300 | seconds | OAuth loopback wait. |

### State store

| Constant | Value | Units | Notes |
|---|---|---|---|
| `STATE_POOL_SIZE` | 4 | connections | rusqlite pool. |
| `AUDIT_RETENTION_DAYS` | 30 | days | |
| `RUN_HISTORY_RETENTION_DAYS` | 90 | days | |
| `CONFLICT_RETENTION_DAYS` | 180 | days | Resolved conflicts only. |
| `LOG_ROTATE_BYTES` | 32 * MiB | bytes | JSONL rotation threshold. |
| `LOG_RETAIN_FILES` | 10 | files | Past-rotation files kept. |

### IPC and lifecycle

| Constant | Value | Units | Notes |
|---|---|---|---|
| `IPC_FRAME_MAX_BYTES` | 1 * MiB | bytes | Single JSON-RPC frame cap. |
| `IPC_KEEPALIVE_MS` | 30_000 | ms | Subscription liveness ping. |
| `SUB_GC_INTERVAL_MS` | 60_000 | ms | Dead-subscription sweep. |
| `INSTALL_TIMEOUT_S` | 60 | seconds | `health.ping` poll after install. |
| `SHUTDOWN_DRAIN_TIMEOUT_S` | 30 | seconds | Graceful shutdown drain. |
| `UPGRADE_DRAIN_TIMEOUT_S` | 30 | seconds | Upgrade drain. |
| `MAX_RUNTIME_WORKERS` | min(num_cpus, 4) | workers | Tokio runtime size. |
| `MAX_CLOCK_SKEW_S` | 600 | seconds | Skew tolerance for remote-mtime comparison. |

Every limit is observable: on reaching one, the daemon emits an audit event whose `kind` is
`limit.<const_name>` with the current and threshold values in the payload. The
[`development guidelines`](https://gist.github.com/antstanley/5bdaa85e63427fadae1c58ae6db77c27)
require this for every named limit.

---

## Code style specifics

- 70-line function cap, 100-column line cap, per the gist.
- Module file size: aim ≤ 400 lines. Larger modules signal an opportunity to split.
- Public items in library crates carry doc comments.
- Doc comments answer "why", not "what". Restate the contract concisely, then explain
  surprising consequences.
- Test names follow the gist pattern `it_<action>_when_<condition>`.
- Imports grouped `std`, then external crates, then `crate::`. `cargo fmt` enforces ordering.

---

## Repository hygiene specifics

- `.gitignore` excludes: `target/`, `.private/`, `*.sqlite`, `*.sqlite-*`, `*.log`, `*.jsonl`,
  `coverage/`, `.idea/`, `.vscode/` (unless explicitly shared via `.vscode/settings.json`
  for project-level config), `.DS_Store`.
- `docs/` is the canonical location for specs and decisions.
- `crates/<name>/src/fakes.rs` holds the in-memory fake for that adapter's port. Fakes ship
  in the same crate so they can use private types, but live behind `#[cfg(any(test,
  feature = "fakes"))]` so they do not bloat release builds.
- Operator credentials, Azure tenant IDs, and any test-tenant secrets live outside the repo
  in a 1Password vault; CI gets them via OIDC at job start.

---

## Pre-commit and pre-push

`jj` (jujutsu) is the version-control tool. The pre-push hook runs:

```
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run --workspace --no-fail-fast
cargo deny check
cargo xtask audit-asserts
```

The `audit-asserts` xtask walks every function in `onesync-core` and counts assertion
statements; below the floor it fails with the function name.

CI runs the same plus the `slow` tier (e2e tests against the real Graph test tenant) on PRs
to `main`.

---

## AI-agent rules

The gist's AI-agent section applies in full. Project-specific reinforcements:

- **No invented limits.** Every numeric threshold must already exist in `limits.rs` or be
  added there in the same change. A literal `5_000` in domain code is a bug.
- **No invented canonical fields.** The JSON Schema sidecar
  ([`canonical-types.schema.json`](canonical-types.schema.json)) is the authoritative
  shape. Adding a field there is part of any change that introduces it in code.
- **No silent error swallowing.** Even in agent-generated code, every `Result` is matched.
- **Ports are the contract.** Adding I/O to `onesync-core` directly is a bug; introduce a
  port.

---

## Definition of done

Inheriting from the gist; restated here so the project owns the bar:

- Behavior exercised by tests (unit, integration, or e2e). For changes touching the engine,
  property tests as well.
- Negative-space tests cover every validation path the change introduces.
- Every new function has 2+ meaningful assertions.
- Every new limit is a named `const` in `limits.rs`, with units in the name, and is
  referenced in [`09-development-guidelines.md`](09-development-guidelines.md) (this page).
- `cargo fmt`, `cargo clippy -D warnings`, `cargo nextest run` all pass locally.
- Schema and types regenerated if any canonical entity changed.
- Commit explains the why.
- PR describes architecture-level changes and links to the affected spec page(s).

---

## Assumptions and open questions

**Assumptions**

- The gist URL is stable. If it moves, this page is updated.
- Rust 1.95.0 features (specifically the async-trait-friendly object safety improvements)
  are sufficient. We do not depend on nightly features.
- macOS 13 is the floor. macOS 12 is unsupported because FSEvents' `noDefer` semantics on
  12 are subtly different.

**Decisions**

- *No TypeScript or Svelte in this repo.* **Pure Rust workspace.** The gist mentions both;
  onesync skips those sections.
- *Compile-time limits, not runtime config.* **Limits are `const`s.** Operator-tunable
  values (log level, metered-network policy, free-space margin) live in
  `InstanceConfig`; everything else is a const. Promotion to operator-tunable requires
  a spec change.
- *`jj` is the VCS.* **Aligned with the gist.** Git-style mirrors are kept available for
  PR review tooling but the source of truth is `jj`.

**Open questions**

- *Cross-compilation strategy.* macOS-only at present; if Linux ever joins, we need a
  shared subset of these constants plus a per-platform limits module.
- *Sustained CI cost of the e2e tier.* Microsoft Graph rate limits and test-tenant quotas
  may push the slow-tier into nightly-only as we grow the suite.
