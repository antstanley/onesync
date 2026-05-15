# onesync remediation plan

**Status:** Draft. Scope-controlled — one commit per finding.
**Date:** 2026-05-15 · **Owner:** Stan · **Scope:** Workspace-wide

> Closes every finding from the 2026-05-15 code reviews under `docs/reviews/`,
> plus the structural completion work the sync-engine review identified.
> Supersedes the M12b path-to-v0.1 plan; M12b tasks A–G are absorbed into the
> remediation phases below.

## Goal

Bring code on `main` into alignment with the design specs at `docs/spec/`,
closing all findings from the 2026-05-15 review reports and finishing the
structural gaps the sync-engine review surfaced — while keeping the workspace
shippable at every checkpoint.

## Inputs

- Seven review reports — source of truth for findings:
  - [`docs/reviews/2026-05-15-auth.md`](../reviews/2026-05-15-auth.md)
  - [`docs/reviews/2026-05-15-graph-io.md`](../reviews/2026-05-15-graph-io.md)
  - [`docs/reviews/2026-05-15-sync-engine.md`](../reviews/2026-05-15-sync-engine.md)
  - [`docs/reviews/2026-05-15-state-store.md`](../reviews/2026-05-15-state-store.md)
  - [`docs/reviews/2026-05-15-fs-local.md`](../reviews/2026-05-15-fs-local.md)
  - [`docs/reviews/2026-05-15-daemon-ipc.md`](../reviews/2026-05-15-daemon-ipc.md)
  - [`docs/reviews/2026-05-15-cli-protocol-keychain-time.md`](../reviews/2026-05-15-cli-protocol-keychain-time.md)
- Product specs under [`docs/spec/`](../spec/) — authoritative for design intent.
- The M12b path-to-v0.1 plan ([`2026-05-13-m12b-path-to-v01.md`](2026-05-13-m12b-path-to-v01.md)),
  superseded; its remaining tasks are absorbed (see *M12b absorption* below).
- Workspace conventions: `README.md`, `clippy.toml`, `rust-toolchain.toml`,
  `Cargo.toml` workspace lints.

## Finding inventory

| Phase | Area | BUG | CONCERN | NIT | Total | Notes |
|---:|---|---:|---:|---:|---:|---|
| RP1 | Sync engine (`onesync-core`) | 14 | 13 | 2 | 29 | + structural completion (missing executor branches, conflict op-group materialisation, delta-token persistence, `FileEntry.synced` post-op update, retry-count durability, concurrency) |
| RP2 | Graph I/O (`onesync-graph` non-auth) | 5 | 5 | 4 | 14 | |
| RP3 | CLI + protocol + keychain + time | 7 | 14 | 13 | 34 | Four crates: `onesync-cli`, `onesync-protocol`, `onesync-keychain`, `onesync-time` |
| RP4 | Local FS (`onesync-fs-local`) | 4 | 11 | 5 | 20 | F1 depends on RP1 extending port error variants |
| RP5 | Daemon + IPC (`onesync-daemon`) | 0 | 11 | 7 | 18 | |
| RP6 | Auth / OAuth (`onesync-graph/auth`) | 3 | 4 | 4 | 11 | |
| RP7 | State store (`onesync-state`) | 0 | 6 | 5 | 11 | |
| | **Total** | **33** | **64** | **40** | **137** | |

## Execution model

- **Workspace:** `/Users/stan/code/onesync` on `main`. No jj workspace, no git worktree.
- **VCS tool:** `jj` exclusively. Never `git` directly. (Memory: `feedback_jj_only`.)
- **Commits:** one per finding (granular, bisect-friendly). Message format:

  ```
  fix(<crate>): <one-line description>

  Closes docs/reviews/<area>.md#F<N>.

  Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
  ```

  Commit-type prefixes: `fix(...)` for BUG, `refactor(...)` for behaviour-preserving CONCERN/NIT, `feat(...)` for new structural pieces, `test(...)` for test-only additions.
- **Push:** every commit pushes to `origin/main` immediately via `jj git push --bookmark main`. No PRs at this stage.

## Remediation phases

Engine-first then surface-by-risk (BUG-severity weight). Within a phase: BUGs → CONCERNs → NITs, blast-radius-ordered within each tier.

1. **RP1 — Sync engine** (`onesync-core`)
2. **RP2 — Graph I/O** (`onesync-graph` non-auth)
3. **RP3 — CLI + protocol + keychain + time** (`onesync-cli`, `onesync-protocol`, `onesync-keychain`, `onesync-time`)
4. **RP4 — Local FS** (`onesync-fs-local`)
5. **RP5 — Daemon + IPC** (`onesync-daemon`)
6. **RP6 — Auth / OAuth** (`onesync-graph/auth`)
7. **RP7 — State store** (`onesync-state`)

## Cross-phase dependencies

- RP1 may extend `onesync-core/ports/*` trait contracts with richer error variants. Adapter-side fixes that emit those richer variants (notably Local FS F1) explicitly wait for the consumer to land in RP1.
- RP1 may add new variants or types to `onesync-protocol` (e.g., expanded conflict op-group). Those land in RP1 commits, not deferred to RP3.
- RP3 covers protocol findings *not* already touched by RP1.
- RP2 and RP4–RP7 are mutually independent and run strictly in the listed order.

## M12b absorption

The M12b path-to-v0.1 plan is superseded by this remediation. Its tasks map as follows:

| M12b task | Description | Absorbed into |
|---|---|---|
| A | `phase_local_uploads` step — local-event drain into engine cycle | **RP1** |
| B | `Initializing` → `Active` state transition on first successful cycle | **RP1** |
| C | Real-world OAuth audit (PKCE against `login.microsoftonline.com`) | **RP6** |
| D | Per-connection subscription writer (`ConnCtx` refactor) | **RP5** |
| E | Token refresh resilience + `ReAuthRequired` propagation | **RP6** (also auth review F1) |
| F | Scheduler-side case-collision rename | **RP1** |
| G | Install-doctor coverage (notifications, free space, metered) | **RP5** (also daemon review F8) |

M12b commits used the `feat(.../M12b): ...` prefix; remediation commits use the format above and reference the review finding ID. The M12b plan file remains in `docs/plans/` for history, annotated **Superseded**.

## Methodology — TDD per finding

For each finding, per `superpowers:test-driven-development`:

1. Re-read the finding in its review report.
2. Locate the cited code; verify the report's claim still holds against current `main`.
3. **Red:** write a failing test that captures the bug or pins the missing behaviour. For bugs, the test reproduces wrong output for a concrete input. For structural gaps, the test asserts the missing post-condition.
4. Run the test — observe the *expected* failure mode, not merely "fails".
5. **Green:** implement the fix.
6. Run the test — observe pass.
7. Run impacted crate tests: `cargo nextest run -p <crate>` — must pass.
8. Run `cargo clippy` on touched files — must be `-D warnings` clean against workspace lints (pedantic + nursery + `panic = deny` etc.).
9. Run `cargo fmt --check`.
10. Commit + push.

**Findings without a meaningful behavioural test** (e.g., wrapping `RefreshToken(pub String)` in `secrecy::Secret` to prevent `Debug` leakage): write the strongest test that fits (e.g., assert `format!("{token:?}")` does not contain the inner value); fall back to fix-only commit if no observable behaviour applies. The commit message states this explicitly.

## Verification gates

End-of-phase gate (all four must pass against the full workspace):

```sh
cargo nextest run --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo run -p xtask -- check-schema
```

Gate failures: fix forward in the current phase before advancing. Pre-existing flakiness identified at baseline is logged in `docs/reviews/baseline.md` and does not gate progress.

**Baseline establishment.** First execution action: run all four gates on `main` at the current head. Record results in `docs/reviews/baseline.md` so I can distinguish "I broke this" from "this was already red". Pre-existing failures that block remediation work get triaged before RP1 starts.

## Block conditions

Execution stops and a `docs/reviews/BLOCKED-RP<N>-F<id>.md` file is written if any of these occur:

- **Spec ambiguity** that materially shapes the fix (e.g., an edge case not specified in `docs/spec/03-sync-engine.md`, and the choice changes user-visible behaviour).
- **Finding turns out to be wrong** on closer inspection — reviewer mis-read or the code already handles the case. The block document records the rebuttal and skips the finding.
- **Fix would break a public contract** not currently in spec (e.g., changing an RPC method signature, or a `canonical-types.schema.json` field).
- **Cross-finding conflict** — two findings prescribe incompatible fixes.
- **A test contradicts an existing test** — indicates a spec/protocol mismatch I shouldn't unilaterally resolve.

Blocked files name the finding, the blocker, what was tried, and the decision needed.

## Out of scope

- Push to GitHub Releases / tag a release (release engineering is unblocked-pending per `2026-05-11-roadmap.md`).
- Open or land PRs.
- Bump dependency versions in `Cargo.toml` unless a specific finding requires it.
- Touch Apple Developer ID / codesign / notarisation paths.
- Modify `docs/spec/*`. Specs are authoritative; spec gaps surface as block documents, not edits.
- Add net-new features beyond what the findings call for.
- Cosmetic refactors not tied to a finding.

## Risk + mitigations

- **Engine ripple.** RP1 extends port traits and protocol enums; downstream crates compile-break. Mitigation: additive variants, no removed methods within RP1; dependent crates adopt the new shape in the same RP1 commit as part of the engine change, not deferred.
- **Long-running session.** Per-finding commits + frequent test gates cap the blast radius of any single bad commit. Pushed commits also mean state is durable on remote if the local session is interrupted.
- **Drift from review reports.** Every commit references the finding ID. Phase verification re-reads the report and confirms each finding has a corresponding commit (or a `BLOCKED` document).
- **Test gaps masking residual bugs.** The engine review specifically flagged that integration tests assert telemetry counters rather than state changes (which is why the bugs shipped). New tests added during remediation must assert observable state, not just metric increments.

## Rollback

- `jj op log` snapshots per operation; can rewind individual changes.
- For a pushed commit, rewinding is destructive on the remote — used only on explicit request. Default: fix forward with a new commit.
- No `--force-push`, no destructive `jj abandon` on pushed changes without explicit ask.

## Success criteria

- Every finding in every review report has a corresponding commit on `main` (or a `BLOCKED-*.md` entry).
- All four verification gates pass on the final head of `main`.
- The seven review reports and this remediation plan are committed to `main`.
- `docs/reviews/baseline.md` documents the pre-remediation state for posterity.
- M12b tasks A–G all show as covered through their absorbed RP commits.

## Source-of-truth links

- Review reports: [`docs/reviews/2026-05-15-*.md`](../reviews/)
- Product specs: [`docs/spec/00-overview.md`](../spec/00-overview.md) … [`docs/spec/09-development-guidelines.md`](../spec/09-development-guidelines.md)
- Canonical types schema: [`docs/spec/canonical-types.schema.json`](../spec/canonical-types.schema.json)
- Workspace lints: `Cargo.toml [workspace.lints]`, `clippy.toml`
- Superseded plan (history): [`docs/plans/2026-05-13-m12b-path-to-v01.md`](2026-05-13-m12b-path-to-v01.md)
- Roadmap (orientation only): [`docs/plans/2026-05-11-roadmap.md`](2026-05-11-roadmap.md)

## Assumptions and open questions

**Assumptions**

- The seven review reports accurately describe code state at commit `8a4be07d` (parent of the prep commit). Drift between report and code is rare; if found, the BLOCKED protocol applies.
- `cargo run -p xtask -- check-schema` catches schema-vs-migration drift today; the state-store review confirms it does.
- Pre-existing M12 / M12.1 commits already on `main` remain; this plan does not unwind them.

**Decisions**

- *Plan, not spec.* **This file lives under `docs/plans/` and uses plan voice.** `docs/spec/` is reserved for canonical product specs per the repo's `spec-creator` convention; the remediation work brings code into alignment with those specs but does not modify them.
- *Phase numbering RP1–RP7.* **Disambiguates remediation phases from M-milestones.** Avoids "Phase 1" colliding with "M1" in casual reading.
- *M12b absorbed, not parallel.* **Pre-existing M12b tasks become findings inside remediation phases.** Two parallel plans for overlapping work would cause commit-naming conflicts and split history.
- *Per-finding commits, immediate push.* **Granular history, durable remote state.** Bisect-friendly; survives mid-session interruption.

**Open questions**

- *Pre-existing test flakiness.* If baseline reveals already-failing tests on `main`, fix as part of remediation, gate the work, or annotate-and-defer? Default: annotate in `docs/reviews/baseline.md` and defer unless the failure blocks a specific finding's fix.
- *Canonical-types schema additions.* If RP1 adds a new `FileOp` variant (e.g., for the conflict op group), the schema at `docs/spec/canonical-types.schema.json` needs a corresponding update. Spec edits are out of scope per *Out of scope* above — this raises a `BLOCKED-RP1-F<id>.md` to surface the spec update to the human owner.
