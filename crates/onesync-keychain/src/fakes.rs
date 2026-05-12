//! In-memory `TokenVault` for tests.

#![cfg(test)]
#![allow(clippy::expect_used)]
// LINT: test-double surface; mutex-poison expects are standard.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use onesync_core::ports::{RefreshToken, TokenVault, VaultError};
use onesync_protocol::{id::AccountId, primitives::KeychainRef};

/// In-memory `TokenVault` backed by a `Mutex<HashMap>`.
#[derive(Default, Debug)]
pub struct InMemoryTokenVault {
    items: Mutex<HashMap<AccountId, String>>,
}

impl InMemoryTokenVault {
    /// Create a new empty [`InMemoryTokenVault`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl TokenVault for InMemoryTokenVault {
    async fn store_refresh(
        &self,
        account: &AccountId,
        token: &RefreshToken,
    ) -> Result<KeychainRef, VaultError> {
        self.items
            .lock()
            .expect("lock")
            .insert(*account, token.0.clone());
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
        let _kc = vault
            .store_refresh(&acct, &RefreshToken("xyz".into()))
            .await
            .expect("store");
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
        let _ = vault
            .store_refresh(&acct, &RefreshToken("v1".into()))
            .await
            .expect("v1");
        let _ = vault
            .store_refresh(&acct, &RefreshToken("v2".into()))
            .await
            .expect("v2");
        let back = vault.load_refresh(&acct).await.expect("load");
        assert_eq!(back.0, "v2");
    }
}
