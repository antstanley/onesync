//! macOS filesystem adapter implementing the `LocalFs` port.
//!
//! See [`docs/spec/05-local-adapter.md`](../../../../docs/spec/05-local-adapter.md).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod adapter;
pub mod error;
#[cfg(any(test, feature = "fakes"))]
pub mod fakes;
pub mod hash;
pub mod ops;
pub mod path;
pub mod scan;
pub mod volumes;
pub mod watcher;
pub mod write;

pub use adapter::LocalFsAdapter;
pub use error::LocalFsAdapterError;
