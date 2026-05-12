//! `SQLite` adapter implementing the `StateStore` port.
//!
//! See [`docs/spec/06-state-store.md`](../../../../docs/spec/06-state-store.md).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod connection;
pub mod error;
pub mod fakes;
pub mod migrations;
pub mod queries;
pub mod retention;
pub mod store;

pub use connection::{ConnectionPool, open};
pub use error::StateStoreError;
pub use store::SqliteStore;
