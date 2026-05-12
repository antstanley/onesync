//! Bridges `TokenVault` to the `TokenSource` shape expected by `onesync-graph`.
//!
//! M3a defines a `TokenSource` trait; the daemon (M5) glues it to a `TokenVault` via this
//! helper. We avoid depending on `onesync-graph` here to keep the crate dependency graph
//! acyclic.

use std::sync::Arc;

use onesync_core::ports::{RefreshToken, TokenVault, VaultError};
use onesync_protocol::id::AccountId;

/// Fetch the refresh token for `account` from `vault`.
///
/// Thin delegation helper — the daemon hands a [`TokenVault`] to the graph adapter via this
/// free function so the two crates stay decoupled.
pub async fn fetch_refresh<V: TokenVault>(
    vault: &V,
    account: &AccountId,
) -> Result<RefreshToken, VaultError> {
    vault.load_refresh(account).await
}

/// Convenience alias: an [`Arc`]-wrapped vault shared across async tasks.
pub type SharedVault<V> = Arc<V>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::InMemoryTokenVault;
    use onesync_protocol::id::{AccountTag, Id};
    use ulid::Ulid;

    fn id() -> AccountId {
        Id::<AccountTag>::from_ulid(Ulid::from(2u128 << 64))
    }

    #[tokio::test]
    async fn fetch_refresh_delegates_to_vault() {
        let vault = InMemoryTokenVault::new();
        let acct = id();
        let _ = vault
            .store_refresh(&acct, &RefreshToken("x".into()))
            .await
            .expect("store");

        let back = fetch_refresh(&vault, &acct).await.expect("fetch");
        assert_eq!(back.0, "x");
    }
}
