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
