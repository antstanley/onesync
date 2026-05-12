//! Stub `TokenVault` for non-macOS builds. Returns `Unsupported` for every operation.
//! Allows the workspace to compile on Linux / Windows for tooling purposes; onesync
//! ships macOS-only.

use async_trait::async_trait;

use onesync_core::ports::{RefreshToken, TokenVault, VaultError};
use onesync_protocol::{id::AccountId, primitives::KeychainRef};

/// `TokenVault` adapter that returns an error on every call (non-macOS builds).
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainTokenVault;

#[async_trait]
impl TokenVault for KeychainTokenVault {
    async fn store_refresh(
        &self,
        _: &AccountId,
        _: &RefreshToken,
    ) -> Result<KeychainRef, VaultError> {
        Err(VaultError::Backend(
            "KeychainTokenVault is macOS-only".into(),
        ))
    }

    async fn load_refresh(&self, _: &AccountId) -> Result<RefreshToken, VaultError> {
        Err(VaultError::Backend(
            "KeychainTokenVault is macOS-only".into(),
        ))
    }

    async fn delete(&self, _: &AccountId) -> Result<(), VaultError> {
        Err(VaultError::Backend(
            "KeychainTokenVault is macOS-only".into(),
        ))
    }
}
