//! Keychain Services-backed `TokenVault`.

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

#[cfg(all(test, target_os = "macos"))]
mod keychain_integration {
    use super::*;
    use onesync_protocol::id::{AccountTag, Id};
    use ulid::Ulid;

    // Each test uses a distinct fixed ULID so real-keychain runs don't collide.
    fn acct_round_trip() -> AccountId {
        Id::<AccountTag>::from_ulid(Ulid::from(0x0001_0000_0000_0000_0000_0000_0000_0001_u128))
    }
    fn acct_not_found() -> AccountId {
        Id::<AccountTag>::from_ulid(Ulid::from(0x0001_0000_0000_0000_0000_0000_0000_0002_u128))
    }
    fn acct_delete_idempotent() -> AccountId {
        Id::<AccountTag>::from_ulid(Ulid::from(0x0001_0000_0000_0000_0000_0000_0000_0003_u128))
    }

    #[tokio::test]
    #[ignore = "touches the real keychain; run explicitly with --ignored"]
    async fn store_then_load_round_trips() {
        let vault = KeychainTokenVault;
        let acct = acct_round_trip();
        let token = RefreshToken("test-refresh-token-12345".into());

        let _kc_ref = vault.store_refresh(&acct, &token).await.expect("store");
        let back = vault.load_refresh(&acct).await.expect("load");
        assert_eq!(back.0, "test-refresh-token-12345");

        // Cleanup
        vault.delete(&acct).await.expect("delete");
    }

    #[tokio::test]
    #[ignore = "touches the real keychain; run explicitly with --ignored"]
    async fn load_returns_not_found_for_unknown_account() {
        let vault = KeychainTokenVault;
        let err = vault
            .load_refresh(&acct_not_found())
            .await
            .expect_err("not found");
        assert!(matches!(err, VaultError::NotFound));
    }

    #[tokio::test]
    #[ignore = "touches the real keychain; run explicitly with --ignored"]
    async fn delete_is_idempotent() {
        let vault = KeychainTokenVault;
        vault
            .delete(&acct_delete_idempotent())
            .await
            .expect("delete on absent");
    }
}
