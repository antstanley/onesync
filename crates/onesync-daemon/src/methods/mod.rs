//! JSON-RPC method handlers.
//!
//! Each sub-module corresponds to one method group.  All handlers receive a
//! [`DispatchCtx`] reference and the raw `params` JSON value, and return a
//! [`serde_json::Value`] on success or a [`MethodError`] on failure.

use std::sync::Arc;
use std::time::Instant;

use onesync_core::ports::StateStore;

pub mod account;
pub mod audit;
pub mod config;
pub mod conflict;
pub mod health;
pub mod pair;
pub mod run;
pub mod service;
pub mod state;

/// Shared context passed to every method handler.
// LINT: `state` is used by account/pair/audit handlers arriving in Task 13.
#[allow(dead_code)]
#[derive(Clone)]
pub struct DispatchCtx {
    /// When the daemon process started (wall clock anchor for uptime).
    pub started_at: Instant,
    /// Access to durable state (accounts, pairs, audit events, …).
    pub state: Arc<dyn StateStore>,
}

/// Application-level method error.
///
/// Handlers return this when they want to surface a specific JSON-RPC
/// application error code (negative, >= [`onesync_protocol::rpc::APP_ERROR_BASE`]).
#[derive(Debug, thiserror::Error)]
#[error("method error {code}: {message}")]
pub struct MethodError {
    /// Application-defined error code.
    pub code: i32,
    /// Human-readable description.
    pub message: String,
}

impl MethodError {
    /// Convenience constructor.
    #[must_use]
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns a `not_implemented` error with [`APP_ERROR_BASE`] code.
    // LINT: used by account/pair/service method stubs arriving in Task 13.
    #[allow(dead_code)]
    #[must_use]
    pub fn not_implemented(method: &str) -> Self {
        Self::new(
            onesync_protocol::rpc::APP_ERROR_BASE,
            format!("method '{method}' is not yet implemented"),
        )
    }
}
