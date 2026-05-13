//! Daemon-side scheduler: long-running tokio task that drives `engine::run_cycle` against
//! every active pair on a fixed interval, plus a force-sync channel for ad-hoc cycles.
//!
//! Architecture: one tokio task owns the scheduler. It does not spawn a separate worker per
//! pair (the existing `onesync-core::engine::scheduler::PairWorker` types are scaffolding for
//! a more ambitious design that we may grow into later). Cycles run sequentially on the
//! scheduler thread; concurrency comes from `MAX_CONCURRENT_OPS` inside `executor::execute`.
//! Sequential per-pair iteration keeps token-refresh and `delta_token` round-tripping simple.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::mpsc;

use onesync_core::limits::{DELTA_POLL_INTERVAL_MS, TOKEN_REFRESH_LEEWAY_S};
use onesync_core::ports::{
    AuditSink, Clock, LocalFs, RefreshToken, StateStore, TokenVault, VaultError,
};
use onesync_graph::adapter::{GraphAdapter, TokenSource};
use onesync_graph::auth::refresh;
use onesync_graph::error::GraphInternalError;
use onesync_protocol::{
    enums::{PairStatus, RunTrigger},
    id::PairId,
};
use onesync_time::UlidGenerator;

use crate::shutdown::ShutdownToken;

/// Capacity of the force-sync mpsc channel.
const FORCE_QUEUE_DEPTH: usize = 64;

/// Handle the daemon retains to inject force-sync triggers into the running scheduler task.
#[derive(Clone)]
pub struct SchedulerHandle {
    force_tx: mpsc::Sender<PairId>,
}

impl SchedulerHandle {
    /// Push a manual force-sync trigger. The scheduler picks it up before the next tick.
    ///
    /// # Errors
    ///
    /// Returns `()` if the scheduler task has already exited (channel closed).
    pub async fn force_sync(&self, pair: PairId) -> Result<(), ()> {
        self.force_tx.send(pair).await.map_err(|_| ())
    }

    /// Construct a no-op handle for tests. Force-sync sends succeed but no work is performed.
    #[must_use]
    pub fn for_tests() -> Self {
        let (force_tx, _force_rx) = mpsc::channel::<PairId>(FORCE_QUEUE_DEPTH);
        Self { force_tx }
    }
}

/// Inputs the scheduler needs. Constructed by `wiring::build_ports` + the daemon main.
#[derive(Clone)]
pub struct SchedulerInputs {
    /// Durable state store.
    pub state: Arc<dyn StateStore>,
    /// macOS filesystem adapter.
    pub local_fs: Arc<dyn LocalFs>,
    /// Wall clock.
    pub clock: Arc<dyn Clock>,
    /// ULID generator for new row identifiers.
    pub ids: Arc<UlidGenerator>,
    /// Audit-event sink.
    pub audit: Arc<dyn AuditSink>,
    /// OAuth refresh-token vault.
    pub vault: Arc<dyn TokenVault>,
    /// Shared HTTP client (rustls; built once).
    pub http: reqwest::Client,
    /// Host name used in conflict-copy filenames.
    pub host_name: String,
}

/// Spawn the scheduler task and return a [`SchedulerHandle`] for force-sync injection.
#[must_use]
pub fn spawn(inputs: SchedulerInputs, shutdown: &ShutdownToken) -> SchedulerHandle {
    let (force_tx, mut force_rx) = mpsc::channel::<PairId>(FORCE_QUEUE_DEPTH);
    let mut shutdown_rx = shutdown.subscribe();

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(DELTA_POLL_INTERVAL_MS));
        // First tick fires immediately; eat it so we do not race startup with `pair.add`.
        tick.tick().await;

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    run_due_pairs(&inputs).await;
                }
                Some(pair_id) = force_rx.recv() => {
                    tracing::info!(pair = %pair_id, "scheduler: force-sync trigger");
                    run_one_pair(&inputs, &pair_id, RunTrigger::CliForce).await;
                }
                _ = shutdown_rx.recv() => {
                    tracing::info!("scheduler: shutdown signal received");
                    break;
                }
            }
        }
    });

    SchedulerHandle { force_tx }
}

/// Tick handler: iterate active pairs and run a `Scheduled` cycle on each non-paused one.
async fn run_due_pairs(inputs: &SchedulerInputs) {
    let pairs = match inputs.state.pairs_active().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "scheduler: pairs_active failed");
            return;
        }
    };
    for pair in pairs {
        if pair.paused || pair.status == PairStatus::Removed {
            continue;
        }
        run_one_pair(inputs, &pair.id, RunTrigger::Scheduled).await;
    }
}

/// Run a single cycle for the named pair. Logs errors; never panics.
pub async fn run_one_pair(inputs: &SchedulerInputs, pair_id: &PairId, trigger: RunTrigger) {
    if let Err(e) = run_one_pair_inner(inputs, pair_id, trigger).await {
        tracing::warn!(pair = %pair_id, error = %e, "scheduler: cycle failed");
    }
}

async fn run_one_pair_inner(
    inputs: &SchedulerInputs,
    pair_id: &PairId,
    trigger: RunTrigger,
) -> anyhow::Result<()> {
    use onesync_core::engine::cycle::CycleCtx;
    use onesync_core::engine::run_cycle;

    // 1. Look up pair + account.
    let pair = inputs
        .state
        .pair_get(pair_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("pair not found: {pair_id}"))?;
    let account = inputs
        .state
        .account_get(&pair.account_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("account not found: {}", pair.account_id))?;

    // 2. Get client_id from config.
    let cfg = inputs.state.config_get().await?;
    let client_id = cfg.map(|c| c.azure_ad_client_id).unwrap_or_default();
    if client_id.is_empty() {
        anyhow::bail!("azure_ad_client_id is unset; refusing to schedule pair {pair_id}");
    }

    // 3. Build a token source backed by the vault + refresh endpoint.
    let token_source = VaultBackedTokenSource::new(
        inputs.http.clone(),
        inputs.vault.clone(),
        account.id,
        client_id,
    );

    let drive_id = account.drive_id.clone();
    // 4. Build a GraphAdapter scoped to this account's drive.
    let remote = GraphAdapter::with_client(inputs.http.clone(), token_source, account.drive_id);

    // 5. Build the CycleCtx and drive run_cycle.
    let ctx = CycleCtx {
        pair_id: pair.id,
        local_root: pair.local_path.clone(),
        drive_id,
        cursor: pair.delta_token.clone(),
        trigger,
        state: inputs.state.as_ref(),
        remote: &remote,
        local: inputs.local_fs.as_ref(),
        audit: inputs.audit.as_ref(),
        clock: inputs.clock.as_ref(),
        ids: inputs.ids.as_ref(),
        host_name: inputs.host_name.clone(),
    };
    let summary = run_cycle(&ctx).await?;
    tracing::info!(
        pair = %pair_id,
        ?trigger,
        items = summary.remote_items_seen,
        ops = summary.ops_applied,
        conflicts = summary.conflicts_detected,
        "scheduler: cycle complete"
    );

    Ok(())
}

/// `TokenSource` that loads a refresh token from a `TokenVault`, calls the Microsoft
/// `/token` endpoint with `grant_type=refresh_token`, and caches the result until it's within
/// `TOKEN_REFRESH_LEEWAY_S` of expiry.
struct VaultBackedTokenSource {
    http: reqwest::Client,
    vault: Arc<dyn TokenVault>,
    account_id: onesync_protocol::id::AccountId,
    client_id: String,
    cache: tokio::sync::Mutex<Option<CachedToken>>,
}

struct CachedToken {
    access_token: String,
    fetched_at: std::time::Instant,
    expires_in: u64,
}

impl VaultBackedTokenSource {
    fn new(
        http: reqwest::Client,
        vault: Arc<dyn TokenVault>,
        account_id: onesync_protocol::id::AccountId,
        client_id: String,
    ) -> Self {
        Self {
            http,
            vault,
            account_id,
            client_id,
            cache: tokio::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl TokenSource for VaultBackedTokenSource {
    async fn access_token(&self) -> Result<String, GraphInternalError> {
        // Fast path: cache hit.
        {
            let guard = self.cache.lock().await;
            if let Some(c) = guard.as_ref() {
                let elapsed = c.fetched_at.elapsed().as_secs();
                if elapsed + TOKEN_REFRESH_LEEWAY_S < c.expires_in {
                    return Ok(c.access_token.clone());
                }
            }
        }
        // Cache miss / expiring soon: refresh.
        let RefreshToken(rt) =
            self.vault
                .load_refresh(&self.account_id)
                .await
                .map_err(|e: VaultError| GraphInternalError::Decode {
                    detail: format!("vault load failed: {e}"),
                })?;
        let tokens = refresh::refresh(&self.http, "common", &self.client_id, &rt).await?;
        // Persist rotated refresh.
        let _ = self
            .vault
            .store_refresh(&self.account_id, &RefreshToken(tokens.refresh_token))
            .await;
        let access = tokens.access_token.clone();
        let new = CachedToken {
            access_token: tokens.access_token,
            fetched_at: std::time::Instant::now(),
            expires_in: tokens.expires_in,
        };
        {
            let mut guard = self.cache.lock().await;
            *guard = Some(new);
        }
        Ok(access)
    }
}
