# onesync M3b — Keychain Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. The workspace for this milestone is `/Volumes/Delorean/onesync-m3b-keychain/`. All commits use `jj describe -m "..."` + `jj new`. **Never invoke `git` directly.**

**Goal:** Build `onesync-keychain` — the macOS Keychain shim that implements the `TokenVault` port. Three methods (`store_refresh`, `load_refresh`, `delete`), backed by `security-framework`. Plus an in-memory `FakeTokenVault` for tests, and a `TokenSource` glue that lets `onesync-graph` retrieve a refresh token by `AccountId` without depending on the keychain crate directly.

**Architecture:** Single small crate (~300 LOC of impl + tests). The macOS-specific code lives behind a `cfg(target_os = "macos")` gate; non-macOS builds compile a stub that returns `Unsupported` errors (this keeps the workspace cross-compile-clean for tooling that runs `cargo check` outside macOS — not required for shipping but useful for CI matrix expansion).

**Tech Stack:** `security-framework` 3.x (wraps Apple's Security framework), `async-trait`, `tokio` (the port trait is async).

VCS: jj-colocated. Per-task commits inside the workspace; the workspace's `@` advances as tasks land. Trailer: `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` verbatim on every commit.

**Parallel milestone:** M3a (`onesync-graph`) runs simultaneously in `../onesync-m3a-graph/`. The two crates touch disjoint source files. The workspace `Cargo.toml` `[workspace.dependencies]` is the only shared point; M3a lands its deps first (in M3a Task 1), and M3b layers `security-framework` on top in M3b Task 1. As long as M3a finishes Task 1 before M3b starts Task 1, no merge friction occurs. If they race, the controller rebases M3b's Task 1 onto M3a's Task 1.

---

## Pre-flight (read before starting)

- M1 + M2 are complete; `origin/main` is at `bab03f67`. Workspace test count: 125.
- This plan executes inside the jj workspace `/Volumes/Delorean/onesync-m3b-keychain/` (named `m3b-keychain`).
- The `TokenVault` port is defined in `onesync-core::ports::token_vault` (M1 Task 12). Three methods:
  - `store_refresh(account, token) -> Result<KeychainRef, VaultError>`
  - `load_refresh(account) -> Result<RefreshToken, VaultError>`
  - `delete(account) -> Result<(), VaultError>`
- Spec: [`docs/spec/04-onedrive-adapter.md`](../spec/04-onedrive-adapter.md) §Token lifecycle: "Refresh tokens live in the macOS Keychain via `TokenVault`. The keychain entry's service name is `dev.onesync.refresh-token` and the account name is the `AccountId` literal."

---

## File map (M3b creates)

```
crates/onesync-keychain/
├── Cargo.toml
└── src/
    ├── lib.rs
    ├── macos.rs       # security-framework-backed TokenVault impl (cfg(target_os = "macos"))
    ├── stub.rs        # cfg(not(target_os = "macos")) — returns Unsupported
    ├── token_source.rs # bridges TokenVault to onesync-graph's TokenSource trait
    └── fakes.rs       # in-memory TokenVault for tests
```

5 tasks total: 4 implementation + 1 close. Workspace test count target: 125 (entry) → ≥ 135 (exit).

---

## Task 1: Crate skeleton + workspace dep

**Files:**
- Modify: `Cargo.toml` — add to `[workspace.dependencies]`:
  ```toml
  security-framework = "3"
  ```
  (May need a `[target.'cfg(target_os = "macos")'.dependencies]` constraint in the crate's `Cargo.toml` so non-macOS builds skip it.)
- Create: `crates/onesync-keychain/Cargo.toml` with:
  ```toml
  [package]
  name = "onesync-keychain"
  version = "0.1.0"
  edition.workspace = true
  rust-version.workspace = true
  license.workspace = true

  [lints]
  workspace = true

  [dependencies]
  onesync-core     = { path = "../onesync-core" }
  onesync-protocol = { path = "../onesync-protocol" }
  async-trait      = { workspace = true }
  thiserror        = { workspace = true }
  tokio            = { workspace = true }

  [target.'cfg(target_os = "macos")'.dependencies]
  security-framework = { workspace = true }

  [dev-dependencies]
  tempfile = { workspace = true }
  ```
- Create: `src/lib.rs`:
  ```rust
  //! macOS Keychain adapter implementing the `TokenVault` port.
  //!
  //! See [`docs/spec/04-onedrive-adapter.md`](../../../../docs/spec/04-onedrive-adapter.md).

  #![forbid(unsafe_code)]
  #![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

  pub mod fakes;
  pub mod token_source;

  #[cfg(target_os = "macos")]
  pub mod macos;
  #[cfg(target_os = "macos")]
  pub use macos::KeychainTokenVault;

  #[cfg(not(target_os = "macos"))]
  pub mod stub;
  #[cfg(not(target_os = "macos"))]
  pub use stub::KeychainTokenVault;
  ```
- Create: module stubs for `macos.rs`, `stub.rs`, `token_source.rs`, `fakes.rs` (doc comments only).

**Gate:** `cargo check -p onesync-keychain`; workspace tests still 125; clippy + fmt clean.
**Commit:** `feat(keychain): onesync-keychain crate skeleton`

---

## Task 2: macOS Keychain `TokenVault` implementation

**Files:** `src/macos.rs`

```rust
//! Keychain Services-backed TokenVault.

use async_trait::async_trait;
use security_framework::passwords;

use onesync_core::ports::{RefreshToken, TokenVault, VaultError};
use onesync_protocol::{id::AccountId, primitives::KeychainRef};

const SERVICE_NAME: &str = "dev.onesync.refresh-token";

/// `TokenVault` adapter backed by the macOS Keychain.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainTokenVault;

#[async_trait]
impl TokenVault for KeychainTokenVault {
    async fn store_refresh(
        &self,
        account: &AccountId,
        token: &RefreshToken,
    ) -> Result<KeychainRef, VaultError> {
        let account_str = account.to_string();
        let secret = token.0.clone();
        tokio::task::spawn_blocking(move || {
            passwords::set_generic_password(SERVICE_NAME, &account_str, secret.as_bytes())
                .map_err(|e| VaultError::Backend(format!("set_generic_password: {e}")))
        })
        .await
        .map_err(|e| VaultError::Backend(format!("join: {e}")))??;

        Ok(KeychainRef::new(account.to_string()))
    }

    async fn load_refresh(&self, account: &AccountId) -> Result<RefreshToken, VaultError> {
        let account_str = account.to_string();
        let bytes = tokio::task::spawn_blocking(move || {
            passwords::get_generic_password(SERVICE_NAME, &account_str)
        })
        .await
        .map_err(|e| VaultError::Backend(format!("join: {e}")))?
        .map_err(|e| match e.code() {
            -25300 => VaultError::NotFound, // errSecItemNotFound
            _ => VaultError::Backend(format!("get_generic_password: {e}")),
        })?;

        let secret = String::from_utf8(bytes)
            .map_err(|e| VaultError::Backend(format!("non-utf8 secret: {e}")))?;
        Ok(RefreshToken(secret))
    }

    async fn delete(&self, account: &AccountId) -> Result<(), VaultError> {
        let account_str = account.to_string();
        tokio::task::spawn_blocking(move || {
            passwords::delete_generic_password(SERVICE_NAME, &account_str).or_else(|e| {
                if e.code() == -25300 {
                    Ok(()) // already absent — idempotent delete
                } else {
                    Err(e)
                }
            })
        })
        .await
        .map_err(|e| VaultError::Backend(format!("join: {e}")))?
        .map_err(|e| VaultError::Backend(format!("delete_generic_password: {e}")))
    }
}
```

`security-framework` 3.x exposes `passwords::set_generic_password` / `get_generic_password` / `delete_generic_password` — the simplest high-level API for storing per-service-and-account secrets. The error code `-25300` is `errSecItemNotFound`.

**Tests** (gated `#[cfg(target_os = "macos")]` AND `#[ignore]` by default because they touch the real keychain):

```rust
#[cfg(all(test, target_os = "macos"))]
mod keychain_integration {
    use super::*;
    use onesync_protocol::id::{AccountTag, Id};
    use ulid::Ulid;

    fn fresh_account_id() -> AccountId {
        // Use a random ULID so tests don't collide.
        Id::<AccountTag>::from_ulid(Ulid::new())
    }

    #[tokio::test]
    #[ignore = "touches the real keychain; run explicitly with --ignored"]
    async fn store_then_load_round_trips() {
        let vault = KeychainTokenVault;
        let acct = fresh_account_id();
        let token = RefreshToken("test-refresh-token-12345".into());

        let _kc_ref = vault.store_refresh(&acct, &token).await.expect("store");
        let back = vault.load_refresh(&acct).await.expect("load");
        assert_eq!(back.0, "test-refresh-token-12345");

        // Cleanup
        vault.delete(&acct).await.expect("delete");
    }

    #[tokio::test]
    #[ignore]
    async fn load_returns_not_found_for_unknown_account() {
        let vault = KeychainTokenVault;
        let err = vault.load_refresh(&fresh_account_id()).await.expect_err("not found");
        assert!(matches!(err, VaultError::NotFound));
    }

    #[tokio::test]
    #[ignore]
    async fn delete_is_idempotent() {
        let vault = KeychainTokenVault;
        vault.delete(&fresh_account_id()).await.expect("delete on absent");
    }
}
```

These tests are `#[ignore]`d so the default gate doesn't trigger keychain prompts. Run explicitly via `cargo nextest run -p onesync-keychain --run-ignored only` on macOS.

The functional verification for this task: the code compiles, clippy is clean, fmt is clean.

**Gate + commit:** `feat(keychain): macOS Keychain Services TokenVault impl`

---

## Task 3: Non-macOS stub

**Files:** `src/stub.rs`

```rust
//! Stub `TokenVault` for non-macOS builds. Returns `Unsupported` for every operation.
//! Allows the workspace to compile on Linux / Windows for tooling purposes; onesync
//! ships macOS-only.

use async_trait::async_trait;

use onesync_core::ports::{RefreshToken, TokenVault, VaultError};
use onesync_protocol::{id::AccountId, primitives::KeychainRef};

#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainTokenVault;

#[async_trait]
impl TokenVault for KeychainTokenVault {
    async fn store_refresh(&self, _: &AccountId, _: &RefreshToken) -> Result<KeychainRef, VaultError> {
        Err(VaultError::Backend("KeychainTokenVault is macOS-only".into()))
    }
    async fn load_refresh(&self, _: &AccountId) -> Result<RefreshToken, VaultError> {
        Err(VaultError::Backend("KeychainTokenVault is macOS-only".into()))
    }
    async fn delete(&self, _: &AccountId) -> Result<(), VaultError> {
        Err(VaultError::Backend("KeychainTokenVault is macOS-only".into()))
    }
}
```

`VaultError` doesn't have an `Unsupported` variant — reuse `Backend(String)` with a clear message. Adding a dedicated variant is a future port-shape decision.

**Gate + commit:** `feat(keychain): non-macOS stub TokenVault`

---

## Task 4: In-memory fake `TokenVault`

**Files:** `src/fakes.rs`

```rust
//! In-memory `TokenVault` for tests.

#![cfg(test)]
#![allow(clippy::expect_used)]
// LINT: test-double surface; mutex-poison expects are standard.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use onesync_core::ports::{RefreshToken, TokenVault, VaultError};
use onesync_protocol::{id::AccountId, primitives::KeychainRef};

#[derive(Default, Debug)]
pub struct InMemoryTokenVault {
    items: Mutex<HashMap<AccountId, String>>,
}

impl InMemoryTokenVault {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl TokenVault for InMemoryTokenVault {
    async fn store_refresh(&self, account: &AccountId, token: &RefreshToken) -> Result<KeychainRef, VaultError> {
        self.items.lock().expect("lock").insert(*account, token.0.clone());
        Ok(KeychainRef::new(account.to_string()))
    }

    async fn load_refresh(&self, account: &AccountId) -> Result<RefreshToken, VaultError> {
        self.items
            .lock()
            .expect("lock")
            .get(account)
            .cloned()
            .map(RefreshToken)
            .ok_or(VaultError::NotFound)
    }

    async fn delete(&self, account: &AccountId) -> Result<(), VaultError> {
        self.items.lock().expect("lock").remove(account);
        Ok(()) // idempotent
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onesync_protocol::id::{AccountTag, Id};
    use ulid::Ulid;

    fn id() -> AccountId {
        Id::<AccountTag>::from_ulid(Ulid::from(1u128 << 64))
    }

    #[tokio::test]
    async fn store_then_load_round_trips() {
        let vault = InMemoryTokenVault::new();
        let acct = id();
        let _kc = vault.store_refresh(&acct, &RefreshToken("xyz".into())).await.expect("store");
        let back = vault.load_refresh(&acct).await.expect("load");
        assert_eq!(back.0, "xyz");
    }

    #[tokio::test]
    async fn load_returns_not_found_for_unknown_account() {
        let vault = InMemoryTokenVault::new();
        let err = vault.load_refresh(&id()).await.expect_err("not found");
        assert!(matches!(err, VaultError::NotFound));
    }

    #[tokio::test]
    async fn delete_is_idempotent_on_absent_entry() {
        let vault = InMemoryTokenVault::new();
        vault.delete(&id()).await.expect("delete absent");
    }

    #[tokio::test]
    async fn store_overwrites_existing_entry() {
        let vault = InMemoryTokenVault::new();
        let acct = id();
        let _ = vault.store_refresh(&acct, &RefreshToken("v1".into())).await.expect("v1");
        let _ = vault.store_refresh(&acct, &RefreshToken("v2".into())).await.expect("v2");
        let back = vault.load_refresh(&acct).await.expect("load");
        assert_eq!(back.0, "v2");
    }
}
```

**Tests:** 4 round-trip / not-found / idempotent / overwrite cases.

**Gate + commit:** `feat(keychain): in-memory fake TokenVault for tests`

---

## Task 5: `TokenSource` bridge + M3b close

**Files:** `src/token_source.rs`

The graph adapter (M3a Task 16) defines a `TokenSource` trait inside `onesync-graph::adapter`. To avoid coupling the two crates, we bridge it here.

Since we can't depend on `onesync-graph` from this crate (would create a cycle if `onesync-graph` ever needs the keychain), the bridge takes a generic form: the daemon (M5) constructs a `KeychainTokenSource<V: TokenVault>` that holds a `TokenVault` and asynchronously fetches the refresh token by `AccountId`. M3a's `TokenSource` trait will be defined to accept exactly that shape.

For M3b, write the helper alone:

```rust
//! Bridges `TokenVault` to the `TokenSource` shape expected by `onesync-graph`.
//!
//! M3a defines a `TokenSource` trait; the daemon (M5) glues it to a `TokenVault` via this
//! helper. We avoid depending on `onesync-graph` here to keep the crate dependency graph
//! acyclic.

use std::sync::Arc;

use async_trait::async_trait;

use onesync_core::ports::{RefreshToken, TokenVault, VaultError};
use onesync_protocol::id::AccountId;

/// Closure-shaped helper: given a `TokenVault`, build something the daemon can hand to
/// `onesync-graph`.
pub async fn fetch_refresh<V: TokenVault>(
    vault: &V,
    account: &AccountId,
) -> Result<RefreshToken, VaultError> {
    vault.load_refresh(account).await
}

/// Type alias for the daemon's convenience.
pub type SharedVault<V> = Arc<V>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::InMemoryTokenVault;
    use onesync_protocol::id::{AccountTag, Id};
    use ulid::Ulid;

    fn id() -> AccountId {
        Id::<AccountTag>::from_ulid(Ulid::new())
    }

    #[tokio::test]
    async fn fetch_refresh_delegates_to_vault() {
        let vault = InMemoryTokenVault::new();
        let acct = id();
        let _ = vault.store_refresh(&acct, &RefreshToken("x".into())).await.expect("store");

        let back = fetch_refresh(&vault, &acct).await.expect("fetch");
        assert_eq!(back.0, "x");
    }
}
```

This is intentionally thin — the meaningful glue happens in M5 (daemon) when the actual `TokenSource` trait from `onesync-graph` exists at the workspace level.

### Close

- Run the gate: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo nextest run --workspace`.
- Workspace test count target: ≥ 135 (4 fake tests + 1 token_source test).
- Update `docs/plans/2026-05-11-roadmap.md` M3 row with M3b's contribution.
- Commit: `docs(plans): record M3b (keychain adapter) completion on roadmap`.
- Do NOT advance `main` — the controller in the main checkout coordinates the M3a + M3b merge.

**Final commit:** `feat(keychain): TokenSource bridge + M3b complete`

---

## Self-review checklist

- [ ] `KeychainTokenVault` (macOS) implements all three `TokenVault` methods.
- [ ] `errSecItemNotFound` (-25300) maps to `VaultError::NotFound`.
- [ ] `delete` is idempotent (no error when entry is absent).
- [ ] Non-macOS stub returns `VaultError::Backend("…macOS-only")`.
- [ ] In-memory fake matches keychain behaviour (store overwrites, delete idempotent, load returns NotFound).
- [ ] Service name is `dev.onesync.refresh-token`, account name is the `AccountId` literal.
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` exits 0.
- [ ] `cargo fmt --all -- --check` exits 0.
- [ ] No `unsafe` in our code (`security-framework` wraps `unsafe` internally; we don't add any).

## Carry-overs

- The integration tests that touch the real keychain (`#[ignore]`) need a way to run on CI without triggering UI prompts. Apple's Keychain Services don't prompt for generic-password access of the same process — should be safe on CI runners. Verified empirically when CI is enabled for M3.
- The `TokenSource` trait will be defined by M3a in `onesync-graph::adapter`. M3b's `token_source.rs` bridge is intentionally agnostic of that trait so M3a and M3b can land independently; the actual wiring lives in M5.
