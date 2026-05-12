//! `TokenVault` port: secure storage for OAuth refresh tokens (backed by the macOS Keychain).

use async_trait::async_trait;
use onesync_protocol::{id::AccountId, primitives::KeychainRef};

/// Errors returned by `TokenVault` operations.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// No entry was found for the requested account.
    #[error("not found")]
    NotFound,
    /// Underlying keychain or vault failure.
    #[error("backend: {0}")]
    Backend(String),
}

/// An OAuth refresh token, opaque to the engine.
pub struct RefreshToken(
    /// The token string itself, as issued by Microsoft Identity.
    pub String,
);

/// Secure storage for OAuth refresh tokens.
#[async_trait]
pub trait TokenVault: Send + Sync {
    /// Persist a refresh token for `account`, returning the opaque keychain handle.
    async fn store_refresh(
        &self,
        account: &AccountId,
        token: &RefreshToken,
    ) -> Result<KeychainRef, VaultError>;
    /// Retrieve the stored refresh token for `account`.
    async fn load_refresh(&self, account: &AccountId) -> Result<RefreshToken, VaultError>;
    /// Remove the stored refresh token for `account`.
    async fn delete(&self, account: &AccountId) -> Result<(), VaultError>;
}
