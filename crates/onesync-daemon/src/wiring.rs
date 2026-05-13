//! Composition root — constructs all port adapters and wires them together.
//!
//! `build_ports` is the single call site that knows about concrete adapter types.
//! The rest of the daemon works only through the port trait objects / concrete
//! time adapters stored in [`DaemonPorts`].

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use onesync_core::ports::{AuditSink, Clock, LocalFs, StateStore, TokenVault};
use onesync_fs_local::LocalFsAdapter;
use onesync_keychain::KeychainTokenVault;
use onesync_protocol::audit::AuditEvent;
use onesync_state::SqliteStore;
use onesync_time::{SystemClock, UlidGenerator};

use crate::login_registry::LoginRegistry;

/// Live port adapters held for the daemon's entire lifetime.
// LINT: fields are consumed by IPC server tasks added in Tasks 11-14.
#[allow(dead_code)]
pub struct DaemonPorts {
    /// `SQLite`-backed persistent state.
    pub state: Arc<dyn StateStore>,
    /// Real macOS filesystem adapter.
    pub local_fs: Arc<dyn LocalFs>,
    /// Wall-clock adapter.
    pub clock: Arc<dyn Clock>,
    /// ULID identifier generator.
    ///
    /// Stored as the concrete `UlidGenerator` because [`onesync_core::ports::IdGenerator`]
    /// has a generic associated method (`new_id<T>`) and therefore cannot be used as a
    /// `dyn` trait object. Callers that need a generic `I: IdGenerator` accept `&UlidGenerator`
    /// directly.
    pub ids: Arc<UlidGenerator>,
    /// No-op audit sink (replaced by the real `DaemonAuditSink` in Task 14).
    pub audit: Arc<dyn AuditSink>,
    /// macOS Keychain-backed OAuth refresh-token vault.
    pub vault: Arc<dyn TokenVault>,
    /// HTTP client shared across method handlers (rustls; built once).
    pub http: reqwest::Client,
    /// In-flight OAuth login sessions, indexed by login-handle string.
    pub login_registry: Arc<LoginRegistry>,
}

/// Discards all audit events. Stands in until Task 14 wires the real sink.
// LINT: constructed inside build_ports which is itself used in tests and future tasks.
#[allow(dead_code)]
struct NoopAuditSink;

impl AuditSink for NoopAuditSink {
    fn emit(&self, _event: AuditEvent) {}
}

/// Construct all adapters.
///
/// `state_dir` is the directory that will hold `onesync.sqlite`.
///
/// # Errors
///
/// Returns an error if the database cannot be opened or migrations fail.
// LINT: called in tests now; wired into async_main in Task 10.
#[allow(dead_code)]
pub fn build_ports(state_dir: &Path) -> anyhow::Result<DaemonPorts> {
    let clock = Arc::new(SystemClock);

    // Open (or create) the SQLite database and run pending migrations.
    let db_path = state_dir.join("onesync.sqlite");
    let now = clock.now();
    let pool = onesync_state::open(&db_path, &now)
        .with_context(|| format!("failed to open state database at {}", db_path.display()))?;
    let state: Arc<dyn StateStore> = Arc::new(SqliteStore::new(pool));

    let local_fs: Arc<dyn LocalFs> = Arc::new(LocalFsAdapter);
    let ids = Arc::new(UlidGenerator::default());
    let audit: Arc<dyn AuditSink> = Arc::new(NoopAuditSink);
    let vault: Arc<dyn TokenVault> = Arc::new(KeychainTokenVault);
    let http = reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .with_context(|| "failed to build HTTP client".to_owned())?;
    let login_registry = Arc::new(LoginRegistry::new());

    Ok(DaemonPorts {
        state,
        local_fs,
        clock,
        ids,
        audit,
        vault,
        http,
        login_registry,
    })
}

#[cfg(test)]
mod tests {
    use onesync_core::ports::IdGenerator as _;
    use onesync_protocol::id::AccountTag;

    use super::*;

    #[test]
    fn build_ports_succeeds_for_temp_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ports = build_ports(tmp.path()).unwrap();
        // Verify the clock is functional.
        let _ = ports.clock.now();
        // Verify the id generator is functional.
        let _id = ports.ids.new_id::<AccountTag>();
    }
}
