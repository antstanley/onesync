# Remediation baseline

**Date:** 2026-05-16T06:39:13Z
**HEAD:** `4de279e4` — `docs: 2026-05-15 code reviews + remediation plan; supersede M12b`
**Parent (pre-prep):** `8a4be07d` — `docs(spec): make 09-development-guidelines self-contained`
**Toolchain:** Rust 1.95.0 (pinned via `rust-toolchain.toml`)
**Test runner:** `cargo-nextest`

Recorded immediately before RP1 begins so subsequent failures can be attributed.

## Gate results

| Gate | Command | Result |
|---|---|---|
| 1 | `cargo fmt --all -- --check` | PASS (exit 0) |
| 2 | `cargo run -p xtask -- check-schema` | PASS (exit 0) |
| 3 | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | PASS (exit 0) |
| 4 | `cargo nextest run --workspace` | PASS (exit 0) — 312 passed, 5 skipped |

All four gates clean. Any subsequent regression on `main` is attributable to remediation work.

## Skipped tests (informational)

5 nextest tests are tagged `#[ignore]` or `#[cfg(...)]`-gated and don't run in the default profile. These are mostly the M9 end-to-end live-account tests (`m9_end_to_end` etc.) per the M12b plan task C, which require real OneDrive credentials.

## Triage policy for pre-existing flakiness

None observed at baseline; all 312 default-profile tests passed deterministically on the first run. No retries were necessary.

If a previously-passing test starts failing during remediation, the cause is the remediation work — fix forward.

## Output log

Full output preserved at `/tmp/onesync-baseline.log` for the duration of the local session. Key signal: every `_exit=0` for all four gates.
