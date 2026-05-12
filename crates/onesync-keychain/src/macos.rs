//! Keychain Services-backed `TokenVault` (macOS only).

/// `TokenVault` adapter backed by the macOS Keychain.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainTokenVault;
