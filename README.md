# onesync

macOS background daemon and CLI for two-way synchronisation between a designated local folder
and a designated folder in OneDrive (Personal or Business). Written in Safe Rust.

Design: [`docs/spec/`](docs/spec/). Roadmap: [`docs/plans/2026-05-11-roadmap.md`](docs/plans/2026-05-11-roadmap.md).

## Build

Requires Rust 1.95.0 (pinned via `rust-toolchain.toml`) and `cargo-nextest`.

```sh
cargo build --workspace
cargo nextest run --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

## Status

M1 — Foundations complete. M2 onward described in the roadmap.
