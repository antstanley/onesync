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

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use onesync_core::ports::{AuditSink, Clock, LocalFs, StateStore, TokenVault};
use onesync_protocol::rpc::JsonRpcNotification;
use onesync_time::UlidGenerator;
use tokio::sync::mpsc;

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
    /// Path of a binary staged by `service.upgrade.prepare`. After
    /// `service.upgrade.commit` triggers the shutdown token, the daemon's main loop
    /// reads this slot and `exec`s into the staged binary. `None` means no upgrade is
    /// pending.
    pub upgrade_staging: Arc<Mutex<Option<PathBuf>>>,
}

/// Per-connection dispatch wrapper.
///
/// Carries the shared [`DispatchCtx`] plus an mpsc sender that pumps
/// `JsonRpcNotification` frames to the connection's outbound writer task. Methods that
/// register subscriptions (`audit.tail`, future `pair.subscribe` / `conflict.subscribe`)
/// hand notifications to this channel; non-streaming methods can ignore it.
///
/// Implements [`Deref`](std::ops::Deref) so existing handlers that read fields like
/// `ctx.state` keep compiling without modification.
#[derive(Clone)]
pub struct ConnCtx {
    /// Shared, process-wide context.
    pub base: DispatchCtx,
    /// Outbound notification sender for this connection.
    pub notif_tx: mpsc::Sender<JsonRpcNotification>,
}

impl ConnCtx {
    /// Construct a new `ConnCtx`.
    #[must_use]
    pub const fn new(base: DispatchCtx, notif_tx: mpsc::Sender<JsonRpcNotification>) -> Self {
        Self { base, notif_tx }
    }

    /// Construct a `ConnCtx` whose notification channel is immediately closed. Useful for
    /// unit tests and one-shot dispatch sites that never push notifications: any handler
    /// that does try to send will see a closed-channel error rather than blocking.
    #[must_use]
    pub fn detached(base: DispatchCtx) -> Self {
        let (tx, _rx) = mpsc::channel(1);
        // Drop the receiver so the sender is immediately closed for any subsequent send.
        Self { base, notif_tx: tx }
    }
}

impl std::ops::Deref for ConnCtx {
    type Target = DispatchCtx;
    fn deref(&self) -> &Self::Target {
        &self.base
    }
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
