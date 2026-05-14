# Contributing to onesync

This guide is for both human contributors and AI coding agents. It explains
where to look first, how the repository is laid out, the rules every change
must follow, and the mechanics of landing work.

## Read first

1. [`docs/spec/00-overview.md`](docs/spec/00-overview.md) — problem,
   non-goals, the shape of the system. Two paragraphs in you'll know whether
   a proposed change fits the project.
2. [`docs/spec/09-development-guidelines.md`](docs/spec/09-development-guidelines.md) —
   the onesync-specific rules layered on top of the cross-project
   [development guidelines gist](https://gist.github.com/antstanley/5bdaa85e63427fadae1c58ae6db77c27).
   These are mandatory, not suggestions.
3. [`docs/plans/2026-05-11-roadmap.md`](docs/plans/2026-05-11-roadmap.md) —
   the milestone map and current status. Each milestone has a detailed plan
   under [`docs/plans/`](docs/plans/) showing how it actually shipped.

For changes that touch a particular subsystem, read the matching spec page
before writing code:

| Subsystem | Spec |
|---|---|
| Entities, IDs, state machines | [`docs/spec/01-domain-model.md`](docs/spec/01-domain-model.md) |
| Crate layout, ports, hexagonal layering | [`docs/spec/02-architecture.md`](docs/spec/02-architecture.md) |
| Sync cycle, reconcile, conflict, retry | [`docs/spec/03-sync-engine.md`](docs/spec/03-sync-engine.md) |
| Microsoft Graph + OAuth | [`docs/spec/04-onedrive-adapter.md`](docs/spec/04-onedrive-adapter.md) |
| FSEvents, hashing, atomic writes | [`docs/spec/05-local-adapter.md`](docs/spec/05-local-adapter.md) |
| SQLite schema + migrations | [`docs/spec/06-state-store.md`](docs/spec/06-state-store.md) |
| CLI commands + JSON-RPC 2.0 contract | [`docs/spec/07-cli-and-ipc.md`](docs/spec/07-cli-and-ipc.md) |
| LaunchAgent + install/upgrade/uninstall | [`docs/spec/08-installation-and-lifecycle.md`](docs/spec/08-installation-and-lifecycle.md) |
| Canonical JSON Schema for every payload | [`docs/spec/canonical-types.schema.json`](docs/spec/canonical-types.schema.json) |

If a spec page does not yet describe what you need, raise it before writing
code — specs are the source of truth for behaviour.

## Repository layout

```
onesync/
├── crates/
│   ├── onesync-protocol/    canonical types — every spec entity as a serde struct/enum
│   ├── onesync-core/        engine + ports — pure logic, no I/O of its own
│   ├── onesync-state/       SQLite-backed StateStore (rusqlite + refinery migrations)
│   ├── onesync-fs-local/    macOS filesystem adapter (FSEvents + BLAKE3 + atomic writes)
│   ├── onesync-graph/       Microsoft Graph adapter (PKCE auth, /delta, uploads, downloads)
│   ├── onesync-keychain/    macOS Keychain adapter for refresh tokens
│   ├── onesync-time/        SystemClock + UlidGenerator + test fakes
│   ├── onesync-daemon/      onesyncd binary — IPC server, scheduler, audit fan-out
│   └── onesync-cli/         onesync binary — clap surface + JSON-RPC client
├── docs/
│   ├── spec/                design specs (the authoritative behaviour reference)
│   ├── plans/               per-milestone implementation plans + roadmap
│   └── install/             operator-facing install + upgrade + Homebrew + curl|bash
├── xtask/                   workspace task runner (check-schema, dump-schema)
├── .github/workflows/       ci.yml (gate) + release.yml (tagged builds)
├── rust-toolchain.toml      pinned Rust 1.95.0
└── README.md                operator-facing landing page
```

The hexagonal layering is strict: `onesync-core` defines port traits;
adapter crates implement them; `onesync-daemon` wires them together. Don't
introduce I/O into `onesync-core`.

## Toolchain

- Rust **1.95.0** (pinned in `rust-toolchain.toml`).
- `cargo-nextest` for the test runner.
- `cargo-llvm-cov` for coverage (optional).
- **VCS is jujutsu (jj)**, colocated with git. Use `jj` for branches,
  bookmarks, and history rewrites; never call `git` directly on this
  repository. The remote is `origin` and the canonical branch bookmark is
  `main`.

## Before sending a change

Every change must pass the same gates CI runs. Run them locally:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
cargo run -p xtask -- check-schema
```

If `cargo fmt --all -- --check` complains, run `cargo fmt --all`. If
`xtask check-schema` complains, run `cargo run -p xtask -- dump-schema` to
refresh `crates/onesync-state/schema.sql` from the embedded migrations.

When touching the schema:
- Add a new `crates/onesync-state/src/migrations/Vxxx_…sql` file (refinery
  numbers monotonically). Never edit an existing migration.
- Re-run `dump-schema` and commit the refreshed `schema.sql`.
- Update the affected entity in `canonical-types.schema.json` and the spec
  page where the entity lives.

When touching audit events: every limit reached must emit
`limit.<const_name>`; the assertion is part of the
[09-development-guidelines](docs/spec/09-development-guidelines.md) rules.

## Commit and history conventions

- One focused commit per logical change. Each milestone's plan shows the
  granularity (typically one commit per task).
- Commit message subject is conventional: `feat(crate/Mxx): subject`,
  `fix(crate): subject`, `test(crate): subject`, `docs(area): subject`,
  `ci: subject`, `chore: subject`.
- Body explains the **why** in 2–6 lines. Reference the spec / decision
  block / plan task that motivated the change.
- Every commit ends with the Co-Authored-By trailer when authored or
  significantly drafted by an AI agent. Use the exact form:

  ```
  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  ```

  Update the model identifier to match the actual agent.
- Pushes to `main` go through the CI gate before they're considered landed.
  Never force-push to `main`.

## Working in isolation (humans + agents)

For non-trivial work, do it in an isolated jj workspace under
`/Volumes/Delorean/code/onesync-<slug>` (per the agent memory convention)
rather than in the primary checkout. The workflow:

```sh
jj new main                                     # clean shared base
jj workspace add -r main /Volumes/Delorean/code/onesync-<slug>
cd /Volumes/Delorean/code/onesync-<slug>
# … iterate …
jj describe -m "feat(…): …"; jj new            # one commit per task
```

When the stack is ready, fast-forward `main` and tear the workspace down:

```sh
jj bookmark move main --to <tip>
jj workspace forget onesync-<slug> && rm -rf /Volumes/Delorean/code/onesync-<slug>
jj git push --bookmark main
```

## Adding a new milestone

1. Write a plan at `docs/plans/<YYYY-MM-DD>-m<N>-<slug>.md` using the
   `superpowers:writing-plans` skill conventions if you're using that
   toolkit; otherwise follow the structure of the existing plans (scope,
   files, key tests, exit criteria).
2. Update `docs/plans/2026-05-11-roadmap.md` with the new milestone row
   plus its dependencies.
3. Land the work, then update the milestone's `Status:` line on the roadmap
   to `Complete` with the merge commit ref.

## Reporting bugs and security issues

- Functional bugs and feature ideas: open a GitHub issue.
- Security issues touching credentials, the Keychain integration, or the
  PKCE flow: do not open a public issue. Email the maintainer listed in
  `Cargo.toml`'s package metadata.

## For AI agents specifically

Some additional context that helps an agent be productive here without
asking the user 30 questions first:

- **Read the roadmap before proposing work.** Every reasonable next step is
  either already documented as a milestone or as a deferred carry-over in
  one of the existing milestone plans. Don't invent new directions; pick a
  documented task.
- **Trust the specs over the code.** When the two disagree, the spec is
  authoritative until the team agrees to revise it. If a discrepancy is the
  bug you're fixing, name that explicitly in the commit body.
- **Use ports, not adapters.** New core logic depends on the `onesync-core`
  port traits, never on `reqwest`, `rusqlite`, or `security-framework`
  directly. Adapter changes belong in the adapter crate, not the engine.
- **Audit events are the user-visible record.** When you add a code path
  that can fail or produce a notable transition, emit an `AuditEvent` (see
  `crates/onesync-core/src/engine/observability.rs` for the existing
  vocabulary) and reference it in the commit body.
- **Verify before claiming done.** Local `cargo fmt`, `clippy`, `nextest`,
  and `xtask check-schema` must all pass before pushing. The CI gate on
  macOS is identical and will fail otherwise.
- **Keep cycles tight.** Land one task per commit; rebase rather than
  amend; describe the change before pushing so reviewers can read it in
  isolation. The roadmap and per-milestone plans show the historical
  granularity — match it.
