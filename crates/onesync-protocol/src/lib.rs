//! Canonical onesync domain types.
//!
//! Every public type round-trips through `serde_json` and validates against
//! [`docs/spec/canonical-types.schema.json`](../../../docs/spec/canonical-types.schema.json).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod account;
pub mod audit;
pub mod config;
pub mod conflict;
pub mod enums;
pub mod errors;
pub mod file_entry;
pub mod file_op;
pub mod file_side;
pub mod id;
pub mod pair;
pub mod path;
pub mod primitives;
pub mod sync_run;
