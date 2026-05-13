//! JSON-RPC method handlers.
//!
//! Each sub-module corresponds to one method group.  All handlers receive a
//! [`DispatchCtx`] reference and the raw `params` JSON value, and return a
//! [`serde_json::Value`] on success or a [`MethodError`] on failure.

// LINT: handlers share a single async signature so they're dispatchable from one async match.
// Stubs and pure-CPU helpers don't await, which clippy would otherwise flag.
#![allow(clippy::unused_async)]
// LINT: `match Some(x) { ... }` is the readable shape for these one-off Option branches.
#![allow(clippy::option_if_let_else)]

use std::sync::Arc;
use std::time::Instant;

use onesync_core::ports::{AuditSink, Clock, LocalFs, StateStore, TokenVault};
use onesync_time::UlidGenerator;

use crate::ipc::subscriptions::SubscriptionRegistry;
use crate::login_registry::LoginRegistry;
use crate::scheduler::SchedulerHandle;
use crate::shutdown::ShutdownToken;

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
///
/// Carries the port set the methods need to read/write state, emit audit
/// events, allocate identifiers, and observe wall-clock time. Methods that
/// need adapters not on this struct (e.g. the per-account `RemoteDrive` for
/// OAuth or `pair.force_sync`) construct them on demand from `wiring`.
#[derive(Clone)]
pub struct DispatchCtx {
    /// When the daemon process started (wall clock anchor for uptime).
    pub started_at: Instant,
    /// Durable state.
    pub state: Arc<dyn StateStore>,
    /// macOS filesystem adapter (used by pair.add for local-path validation).
    pub local_fs: Arc<dyn LocalFs>,
    /// Wall-clock adapter for stamping new rows.
    pub clock: Arc<dyn Clock>,
    /// ULID generator for new row identifiers.
    pub ids: Arc<UlidGenerator>,
    /// Audit-event sink.
    pub audit: Arc<dyn AuditSink>,
    /// Secure storage for OAuth refresh tokens.
    pub vault: Arc<dyn TokenVault>,
    /// HTTP client shared across method handlers (rustls; built once).
    pub http: reqwest::Client,
    /// In-flight OAuth login sessions, indexed by login-handle string.
    pub login_registry: Arc<LoginRegistry>,
    /// Shutdown token; `service.shutdown` triggers it.
    pub shutdown_token: ShutdownToken,
    /// Daemon state directory; used by `state.backup` / `state.repair.permissions`.
    pub state_dir: std::path::PathBuf,
    /// Handle to the engine scheduler; `pair.force_sync` pushes triggers via this.
    pub scheduler: SchedulerHandle,
    /// Process-global subscription registry; `audit.tail` registers here.
    pub subscriptions: SubscriptionRegistry,
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
