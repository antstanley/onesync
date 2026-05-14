# 09 — Development Guidelines (onesync)

**Status:** Stable · **Date:** 2026-05-14 · **Owner:** Stan

These are the rules of the road for everyone — humans and AI agents — writing code in
this repository. They are deliberately tight; if a guideline doesn't fit a situation,
the answer is to discuss and amend the guideline rather than ignore it.

---

## Philosophy

onesync adopts [TigerBeetle's Tiger Style](https://github.com/tigerbeetle/tigerbeetle/blob/main/docs/TIGER_STYLE.md)
as its pervasive coding style, adapted to Rust. This is not a recommendation; it is the
default. Deviations require a written reason in the commit body.

The short form: **be defensive and validate everything**. Assume any input you did not
produce is wrong. Assume any invariant you did not assert can be violated. Make every
limit explicit, every error handled, every assumption checked.

Tiger Style's design priorities — **safety, performance, developer experience, in that
order** — apply here. When the three pull in different directions, safety wins.

Load-bearing principles, restated in the onesync context:

- **Zero technical debt.** Do it right the first time. Shipping a sound foundation is
  the only sustainable rate of progress.
- **Simple, explicit control flow.** No recursion (use iteration with an explicit
  bound). No clever combinator chains that hide branches. Linear-flow `match` beats
  chained `?` when the chain hides non-trivial control flow.
- **Limits on everything.** Every loop, queue, retry, cache, and payload size has an
  explicit, declared upper bound — see the named-limits tables below. An unbounded
  loop must be `assert!`-bounded by an invariant inside the body.
- **Assertions are first-class code.** They detect programmer errors. The only correct
  response to a violated assertion is to crash. Aim for an average of at least two
  assertions per function (preconditions, postconditions, invariants).
- **Always say *why*.** Comments and commit descriptions explain the rationale, not the
  action. The action is in the code.

---

## Language scope

- **Rust only.** No TypeScript, no Svelte, no shell scripting outside `xtask` helpers
  and the installer script in `docs/install/`.
- **`#![forbid(unsafe_code)]`** is the crate-level default. The only crate that opts
  out is `onesync-keychain` (it wraps `security-framework`, which uses `unsafe`
  internally; our own code never adds `unsafe` blocks). Any opt-out crate adds a
  `// SAFETY:` comment over every `unsafe` block it owns and is review-mandatory.
- **No `std::env::var` outside `onesync-daemon`'s startup module.** Configuration goes
  through the `InstanceConfig` row; environment is for path overrides only.

---

## Toolchain

| Tool | Version / channel | Notes |
|---|---|---|
| Rust | stable, **1.95.0** | pinned in `rust-toolchain.toml` |
| Targets | `aarch64-apple-darwin`, `x86_64-apple-darwin` | CI builds both |
| `cargo-nextest` | latest stable | mandatory for tests; `cargo test` is not used |
| `cargo-llvm-cov` | latest | coverage; 50% floor, target higher per crate |
| `cargo deny` | latest | advisory DB up to date, licenses on the allow list |
| jujutsu (`jj`) | latest | version control; see Version control below |

`rustfmt` (default channel) and `clippy --all-targets --all-features` run in CI and
as a pre-push hook.

---

## Defensive coding and assertions

### Where to validate

| Boundary | What to validate | How |
|---|---|---|
| CLI / IPC request → handler | Body shape, sizes, IDs, enums | Schema validation against `canonical-types.schema.json` (or its derived Rust types) before the handler sees the data |
| Adapter → core | Domain invariants | `assert!` at the top of every core function on its preconditions |
| Core → adapter | Adapter contract | `assert!` on adapter return shapes (e.g. "delta page items have non-empty names") |
| Disk / SQLite read | Round-trip integrity | Schema validation on read, even if we wrote the same shape — pair with the write site |
| Microsoft Graph response | Status, content-type, body shape | Treat third parties as adversarial; never `serde_json::from_slice` and trust the result |
| FSEvents event | Type discriminant + payload shape | Reject unknown event kinds; do not "best-effort" parse |

### Assertion rules

- Use `assert!`, `debug_assert!`, and `assert_eq!` liberally in `onesync-core` code.
  Production builds run with `assert!` enabled (no `--release`-only assertions for
  invariants).
- **Average two or more `assert!`s per function** in `onesync-core`. Preconditions on
  entry, postconditions on exit, invariants in the middle. Empty assertions
  (`assert!(true)`) are not counted. The `cargo xtask audit-asserts` lint enforces this
  threshold and fails the build below it.
- **Pair assertions.** For every property worth enforcing, find at least two independent
  code paths to enforce it. Example: assert manifest validity at write time *and*
  re-validate at every read site that mutates state from it.
- **Assert positive AND negative space.** What you expect, *and* what you do not
  expect. The boundary between the two is where the interesting bugs live.
- **Compile-time assertions** for size and layout invariants:
  `const _: () = assert!(SIZE_OF_FOO == 32);` for any layout the codebase relies on.
- **Split compound assertions.** `assert!(a); assert!(b);` over `assert!(a && b);` —
  failures point at the actual broken condition.
- **Single-line implications.** `if a { assert!(b); }` reads as "a implies b".
- **`unwrap()`, `expect()`, and `panic!()` are forbidden outside `#[cfg(test)]` code
  and outside crate-root `main()` early-exit paths that fail with `EX_CONFIG`.**
- **No `panic!` for control flow.** Panics signal programmer error only.

### Errors are data, not exceptions

- One `Error` enum per crate, via `thiserror`. Variants name the failure category.
- Port-level errors live in `onesync-core` next to the trait that returns them.
- Errors crossing the IPC are serialised through `onesync-protocol`'s `RpcError`;
  adapter internals never leak past the daemon.
- No `Box<dyn Error>` in production paths. Test helpers may use `anyhow` to simplify
  fixture wiring.
- **Every error is handled or explicitly propagated.** Swallowing an error is a bug.
- **Retry policies are explicit and bounded** — max attempts, backoff schedule,
  jitter; see `RETRY_*` constants in `limits.rs`.
- **Never log a secret.** Errors that carry data must scrub anything that could be a
  credential.

### Make invalid states unrepresentable

- Use the type system. `Id<Tag>` newtypes (in `onesync-protocol`) catch
  wrong-id-type bugs at compile time. The same pattern for `Cents`, `Seconds`,
  `Bytes`, etc.
- Enums for state, not strings. `FileSyncState` is the canonical example — match
  exhaustively, no fallthrough.
- Pre-validated string newtypes (`AbsPath`, `RelPath`, `ETag`) for any string with
  structure.

---

## IDs and time

- IDs from `IdGenerator`. Never `Ulid::new()` directly outside that adapter or
  `#[cfg(test)]` code.
- Time from `Clock`. Never `chrono::Utc::now()` or `std::time::SystemTime::now()` in
  core logic.
- Stored timestamps are ISO-8601 UTC strings in the database, `Timestamp` newtype in
  code.

---

## Code style

### Hard limits

- **70 lines per function.** No exceptions. If a function is longer, split it.
  Extract pure helpers; centralise control flow in the parent ("push `if`s up, push
  `for`s down").
- **100 columns per line.** Enforced by `rustfmt`'s `max_width = 100`.
- **Aim ≤ 400 lines per module file.** Larger files signal an opportunity to split.

### Rust conventions

- **`cargo fmt --all`** clean before commit.
- **`cargo clippy --all-targets --all-features -D warnings`** clean before commit.
  Opt-outs require a `LINT:` comment explaining why.
- **Modules over files.** Prefer many small files over large ones.
- **No business logic in `main.rs` or in JSON-RPC handlers.** Handlers parse,
  validate, call into a core function, and serialise the result.
- **No recursion.** Use iteration with an explicit upper bound. The handful of
  unavoidable cases (parsing recursive data) declare the bound at the entry point
  and assert it.
- **Explicit fixed-width integer types** (`u32`, `u64`, `i32`, `i64`) for domain
  values. Avoid `usize`/`isize` for anything that crosses a serialisation boundary.
- **Trait objects for ports;** `&dyn Trait` or `Arc<dyn Trait>`. Generics are fine
  for hot-path ports if a measurement justifies it.
- **`#[must_use]`** on `Result` and on builders.
- **Simpler return types win.** `()` > `bool` > `u64` > `Option<T>` > `Result<T, E>`.
  Chains of `.map().and_then().ok_or()` that hide branches are smells; prefer
  explicit `match` when the control flow is non-trivial.
- **Pass large structs by reference.** If a parameter is `> 16` bytes and not meant
  to be moved, take `&T`.
- **Calculate variables close to their use.** Don't introduce locals far from where
  they're consumed. Don't keep dead bindings around.
- **No duplicated state.** No aliasing of variables. State has one home.
- **Split compound conditions.** `if a { if b { ... } }` over `if a && b { ... }`
  when the conditions check different things.
- **State invariants positively.** `if index < length` over `if index >= length`
  (when expressing the holding case).
- **Brace every `if`** unless it fits entirely on one line.
- **No what-comments.** Comments explain *why*: a non-obvious constraint, a
  workaround for a specific bug, an invariant a future reader would otherwise miss.
  Comments are full sentences with capitalisation and punctuation.
- **Imports** grouped `std`, then external crates, then `crate::`; `cargo fmt`
  enforces ordering.

### Naming

- `snake_case` for functions, variables, modules, files.
- `CamelCase` for types and traits. Acronyms in proper case in `CamelCase` types:
  `HttpClient`, not `HTTPClient`; `OAuthSession`, not `OAUTHSession`.
- **No abbreviations** in identifier names. Exceptions: standard short names
  accepted by the ecosystem (`ctx`, `cfg`, `id`, `i`/`j`/`k` as loop counters).
- **Units last in identifiers**, sorted by descending significance:
  `latency_ms_max`, not `max_latency_ms`. `bytes_max`, `rows_count`,
  `seconds_elapsed`. Related variables sort and align.
- **Same-length names for related variables** where reasonable: `source` / `target`,
  not `src` / `dst`. Aligned source helps the eye spot asymmetry.
- **Helpers prefix with parent name**: `read_sector_callback`,
  `provision_instance_step_two`. Shows call history.
- **Callbacks go last** in parameter lists.
- **Order matters.** A file reads top-down: `main` first; structs before their
  methods; fields before nested types before methods inside a struct module.

### Documentation

- Public items in library crates carry doc comments.
- Each library crate's `lib.rs` carries a top-level doc explaining what the crate
  is, the ports it depends on, and the surface it offers.
- Doc comments answer "why", not "what". Restate the contract concisely, then
  explain surprising consequences.
- No bare `// TODO` without an owner and a tracking link (issue or PR ref).

---

## Testing

### Pyramid

- **Unit tests** live next to the code in `#[cfg(test)]` modules; pure-function
  logic; fast (sub-second per test).
- **Integration tests** live under `crates/<adapter>/tests/`. They exercise core +
  in-memory adapters (from `fakes.rs`) or the real adapter for the system under
  test (e.g. SQLite for `onesync-state`, `tempfile` directories for
  `onesync-fs-local`). Should still run in seconds.
- **End-to-end tests** drive the daemon binary, talk over the IPC socket, and hit
  real macOS or Microsoft Graph services. They are `#[ignore]`'d so they only run
  on explicit opt-in (`cargo test -- --ignored`) and on CI's slow tier.
- **Property tests** (`proptest`) for: path normalisation, conflict naming, retry
  backoff bounds, delta cursor monotonicity, file-entry state machine.
- **Mandatory negative-space tests** for: every limit (1-below, at, 1-above), every
  non-retryable error category, every malformed-input path on the IPC.

### Conventions

- **Test names follow `it_<action>_when_<condition>`** where the structure helps,
  or the standard Rust `descriptive_phrase_with_underscores` where it doesn't.
- **Test data** is built from small builders, not megabyte fixtures.
- **Deterministic time and IDs.** Tests inject `Clock` and `IdGenerator` fakes; no
  `Instant::now()` or random in test bodies.
- **Test data at the validity boundary.** Every limit gets "1 below", "at", and
  "1 above" cases.
- **Positive and negative space together.** A new feature ships with tests for what
  it accepts *and* what it rejects.
- **No flaky tests.** A flaky test is a bug to fix immediately, not a known issue
  to retry around.

### Coverage

CI enforces a **50% line-coverage floor** via `cargo llvm-cov` on the slow tier as
a safety net. Coverage is a floor, not a target — behaviour coverage via
integration tests is what we actually care about. Gaming the floor with trivial
tests is a code-review red flag.

---

## Version control: jujutsu

onesync uses [jujutsu](https://github.com/martinvonz/jj) on top of a Git backend.
Practical norms:

- **Commits are small and well-described.** Aim for a single coherent change per
  commit. The roadmap and per-milestone plans show the historical granularity —
  match it.
- **Empty descriptions are not accepted.** `jj describe` before pushing.
- **Conventional Commits** for the first line of every commit message:
  `type(scope): subject` with types from the standard set (`feat`, `fix`, `docs`,
  `chore`, `refactor`, `test`, `build`, `ci`, `perf`, `style`).
- **Co-Authored-By trailer** when authored or significantly drafted by an AI agent.
  Use the exact form `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`,
  updating the model identifier to match the actual agent.
- **Conflict resolution is in `jj`, not in plain-text markers.** Prefer `jj resolve`
  workflows.
- **Branch model.** `main` is the integration branch; the bookmark name is `main`.
  Feature work happens in isolated jj workspaces (see CONTRIBUTING.md).
- **Do not rewrite published history.** Never force-push to `main`.

For agents specifically: do not run `jj abandon`, `jj op restore`,
`jj git fetch --force`, or any other destructive operation without explicit user
confirmation, even if it seems like the cleanest path.

---

## Pre-commit and pre-push

The pre-push hook (or its CI mirror in `.github/workflows/ci.yml`) runs:

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace --no-fail-fast
cargo xtask check-schema
cargo deny check
cargo xtask audit-asserts
```

The `audit-asserts` xtask walks every function in `onesync-core` and counts
assertion statements; below the floor it fails with the function name.

CI runs the same plus the `--ignored` tier (end-to-end tests against the real
Graph test tenant and the real `launchctl`) on PRs to `main`.

---

## Repository hygiene

- `.gitignore` excludes: `target/`, `.private/`, `*.sqlite`, `*.sqlite-*`, `*.log`,
  `*.jsonl`, `coverage/`, `.idea/`, `.vscode/` (unless explicitly shared via
  `.vscode/settings.json` for project-level config), `.DS_Store`.
- `docs/` is the canonical home for specs and decisions.
- `docs/install/` is the operator-facing surface — install, upgrade, Homebrew,
  curl|bash.
- `crates/<name>/src/fakes.rs` holds the in-memory fake for that adapter's port.
  Fakes ship in the same crate so they can use private types, but live behind
  `#[cfg(any(test, feature = "fakes"))]` so they do not bloat release builds.
- Operator credentials, Azure tenant IDs, and any test-tenant secrets live outside
  the repo; CI gets them via GitHub Actions secrets at job start.

---

## onesync-specific named limits

Every limit is a `pub const` in `onesync_core::limits` with units in the name. This
is the complete list at the time of writing; new limits must be added here and to
the const module in the same change.

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
| `MAX_FILE_SIZE_BYTES` | 10 * GiB | bytes | Hard cap on a single file. |
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

Every limit is observable: on reaching one, the daemon emits an audit event whose
`kind` is `limit.<const_name>` with the current and threshold values in the
payload. This is required for every named limit; reaching a hard limit either
rejects the input or back-pressures the producer, never silently drops.

---

## Guidelines for AI agents working on this codebase

These are not different rules; they are emphasis on places agents tend to slip.

1. **Tiger Style applies to you too.** Defensive validation and explicit limits are
   not optional, even on a "small" change.
2. **Read the roadmap before proposing work.** Every reasonable next step is either
   already documented as a milestone or as a deferred carry-over in one of the
   existing milestone plans. Don't invent new directions; pick a documented task.
3. **Trust the specs over the code.** When the two disagree, the spec is
   authoritative until the team agrees to revise it. If a discrepancy is the bug
   you're fixing, name that explicitly in the commit body.
4. **Stay inside the architecture.** Adding I/O directly to `onesync-core` is the
   most common slip. If new I/O is needed, define a port in
   `crates/onesync-core/src/ports/` first, then implement it as an adapter, then
   call into it from the core.
5. **Add assertions as you go.** Every function you touch should leave with at
   least two assertions in it. `assert!(true)` doesn't count.
6. **No silent error swallowing.** Every `Result` is handled. Every `match` on an
   enum is exhaustive. No `_ = thing()` in production code.
7. **No invented limits.** Every numeric threshold must already exist in
   `limits.rs` or be added there in the same change. A literal `5_000` in domain
   code is a bug.
8. **No invented canonical fields.** [`canonical-types.schema.json`](canonical-types.schema.json)
   is the authoritative shape. Adding a field there is part of any change that
   introduces it in code.
9. **No backwards-compatibility shims.** If a type changes, change every caller.
   The codebase is small; there is no published API.
10. **Audit events are the user-visible record.** When you add a code path that can
    fail or produce a notable transition, emit an `AuditEvent` (see
    `crates/onesync-core/src/engine/observability.rs` for the existing vocabulary).
11. **Tests run before claiming complete.** "Compiles" is not "works". Run
    `cargo nextest run` and report the actual output.
12. **Test positive and negative space together.** A new feature ships with tests
    for what it accepts *and* what it rejects.
13. **Limits are explicit.** Adding a new loop, queue, retry, cache, or buffer
    means adding a named constant for its bound, in the same change.
14. **Prefer small, frequent commits over rolling up a huge change.** jj makes
    small commits cheap. One focused commit per task.
15. **No what-comments.** Comments explain *why*, in full sentences. Comments are
    rare and valuable.
16. **No new README files / extra docs** unless explicitly requested. The spec
    lives in `docs/spec/`.
17. **Ports are the contract.** When in doubt about a piece of behaviour, pick the
    option that keeps the port surface small.
18. **When you change a port, you change every adapter that implements it.** Don't
    leave half-migrated adapters.
19. **Do not run destructive `jj` operations without explicit confirmation.** This
    includes `jj abandon`, force-push, branch deletion — even if it looks like
    the obvious cleanup.
20. **Do not skip pre-commit / pre-push hooks** (`--no-verify`, `--no-gpg-sign`,
    etc.). If a hook fails, fix the underlying issue.

---

## Definition of done

A change is "done" when:

- The behaviour is exercised by a test (unit, integration, or end-to-end as
  appropriate). For changes touching the engine, property tests as well.
- The change includes **negative-space tests** for every new validation path.
- Every new or touched function in `onesync-core` has at least two meaningful
  assertions.
- Every new bound (loop iteration count, queue depth, retry count, payload size)
  is a named `const` in `crates/onesync-core/src/limits.rs` with units in the
  name, and is referenced in this page.
- `cargo fmt`, `cargo clippy -D warnings`, `cargo nextest run`, and
  `cargo xtask check-schema` all pass locally.
- Schema and types regenerated if any canonical entity changed.
- The commit description states the *why*, in 2–6 lines.
- The commit body links to the affected spec page(s) for architecture-level
  changes.

---

## Assumptions and open questions

**Assumptions**

- Rust 1.95.0 features (specifically the async-trait-friendly object safety
  improvements) are sufficient. We do not depend on nightly features.
- macOS 13 is the floor. macOS 12 is unsupported because FSEvents' `noDefer`
  semantics on 12 are subtly different.
- nextest is universally available on developer machines.

**Decisions**

- *No TypeScript or Svelte in this repo.* **Pure Rust workspace.**
- *Compile-time limits, not runtime config.* **Limits are `const`s.**
  Operator-tunable values (log level, metered-network policy, free-space margin)
  live in `InstanceConfig`; everything else is a const. Promotion to
  operator-tunable requires a spec change.
- *`jj` is the VCS.* Git-style mirrors are kept available for PR review tooling
  but the source of truth is `jj`.
- *Coverage gate.* **50% line-coverage floor** via `cargo llvm-cov` on the slow
  CI tier. Floor, not target.
- *Assertion density measurement.* **CI gate via `cargo xtask audit-asserts`.**
  Below the per-function floor fails the build.
- *Tooling language.* **Rust via `xtask`.** Ad-hoc scripts are Rust subcommands
  of the `xtask` crate, not shell.

**Open questions**

- *Cross-compilation strategy.* macOS-only at present; if Linux ever joins, we
  need a shared subset of these constants plus a per-platform limits module.
- *Sustained CI cost of the e2e tier.* Microsoft Graph rate limits and
  test-tenant quotas may push the slow tier into nightly-only as the suite grows.
