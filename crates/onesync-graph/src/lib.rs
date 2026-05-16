//! Microsoft Graph adapter implementing the `RemoteDrive` port.
//!
//! `onesync-graph` provides:
//! - OAuth 2.0 auth-code + PKCE flow for both `OneDrive` Personal and Business accounts
//! - Delta paging, uploads, downloads, renames, deletes, mkdir
//! - Per-account token-bucket throttling
//! - A mapping layer from Graph error codes to the port-level [`GraphError`] enum
//!
//! The public entry point is [`GraphAdapter`], which implements
//! [`onesync_core::ports::RemoteDrive`].

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod adapter;
pub mod auth;
pub mod client;
pub mod delta;
pub mod download;
pub mod error;
pub mod fakes;
pub mod items;
pub mod ops;
pub mod throttle;
pub mod upload;
pub mod urls;

pub use adapter::{GraphAdapter, GraphAdapterError};
