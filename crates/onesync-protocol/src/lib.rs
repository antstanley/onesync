//! Canonical onesync domain types.
//!
//! Every public type round-trips through `serde_json` and validates against
//! [`docs/spec/canonical-types.schema.json`](../../../docs/spec/canonical-types.schema.json).

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod id;
