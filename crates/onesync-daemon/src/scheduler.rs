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
    AuditSink, Clock, LocalFs, RefreshToken, RemoteDrive, StateStore, TokenVault, VaultError,
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

/// Optional configuration of the webhook receiver.
///
/// `register_subscriptions` checks whether the daemon's notification URL is reachable; if so,
/// it registers Graph `/subscriptions` for every `webhook_enabled = true` pair on startup and
/// unregisters them on shutdown.
#[derive(Clone, Debug)]
pub struct WebhookConfig {
    /// Public HTTPS URL the operator's Cloudflare Tunnel maps to the daemon's local receiver.
    /// Set from `InstanceConfig.webhook_notification_url` when configured.
    pub notification_url: Option<String>,
}

/// Spawn the scheduler task and return a [`SchedulerHandle`] for force-sync injection.
#[must_use]
pub fn spawn(inputs: SchedulerInputs, shutdown: &ShutdownToken) -> SchedulerHandle {
    spawn_with_webhooks(
        inputs,
        shutdown,
        WebhookConfig {
            notification_url: None,
        },
    )
}

/// Variant of [`spawn`] that additionally manages Graph subscriptions for pairs with
/// `webhook_enabled = true`. On startup it registers a subscription for each enabled pair;
/// on shutdown it cleans them up.
#[must_use]
pub fn spawn_with_webhooks(
    inputs: SchedulerInputs,
    shutdown: &ShutdownToken,
    webhook_cfg: WebhookConfig,
) -> SchedulerHandle {
    let (force_tx, mut force_rx) = mpsc::channel::<PairId>(FORCE_QUEUE_DEPTH);
    let mut shutdown_rx = shutdown.subscribe();

    tokio::spawn(async move {
        // Registered subscription ids keyed by pair id, so we can unsubscribe on shutdown.
        let mut subscriptions: std::collections::HashMap<PairId, String> =
            std::collections::HashMap::new();

        if let Some(url) = webhook_cfg.notification_url.as_deref() {
            register_initial_subscriptions(&inputs, url, &mut subscriptions).await;
        }

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

        // Best-effort cleanup of Graph subscriptions.
        if !subscriptions.is_empty() {
            unregister_subscriptions(&inputs, &subscriptions).await;
        }
    });

    SchedulerHandle { force_tx }
}

async fn register_initial_subscriptions(
    inputs: &SchedulerInputs,
    notification_url: &str,
    out: &mut std::collections::HashMap<PairId, String>,
) {
    let pairs = match inputs.state.pairs_active().await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "scheduler: pairs_active failed during subscription init");
            return;
        }
    };
    for pair in pairs {
        if !pair.webhook_enabled {
            continue;
        }
        let Ok(Some(account)) = inputs.state.account_get(&pair.account_id).await else {
            continue;
        };
        let cfg = inputs.state.config_get().await.ok().flatten();
        let client_id = cfg.map(|c| c.azure_ad_client_id).unwrap_or_default();
        if client_id.is_empty() {
            continue;
        }
        let token_source = VaultBackedTokenSource::new(
            inputs.http.clone(),
            inputs.vault.clone(),
            account.id,
            client_id,
        );
        let remote =
            GraphAdapter::with_client(inputs.http.clone(), token_source, account.drive_id.clone());
        let client_state = pair.id.to_string();
        match remote
            .subscribe(&account.drive_id, notification_url, &client_state)
            .await
        {
            Ok(sub_id) => {
                tracing::info!(pair = %pair.id, sub = %sub_id, "scheduler: graph subscription registered");
                out.insert(pair.id, sub_id);
            }
            Err(e) => {
                tracing::warn!(pair = %pair.id, error = %e, "scheduler: graph subscribe failed");
            }
        }
    }
}

async fn unregister_subscriptions(
    inputs: &SchedulerInputs,
    subscriptions: &std::collections::HashMap<PairId, String>,
) {
    for (pair_id, sub_id) in subscriptions {
        let Ok(Some(pair)) = inputs.state.pair_get(pair_id).await else {
            continue;
        };
        let Ok(Some(account)) = inputs.state.account_get(&pair.account_id).await else {
            continue;
        };
        let cfg = inputs.state.config_get().await.ok().flatten();
        let client_id = cfg.map(|c| c.azure_ad_client_id).unwrap_or_default();
        if client_id.is_empty() {
            continue;
        }
        let token_source = VaultBackedTokenSource::new(
            inputs.http.clone(),
            inputs.vault.clone(),
            account.id,
            client_id,
        );
        let remote = GraphAdapter::with_client(inputs.http.clone(), token_source, account.drive_id);
        if let Err(e) = remote.unsubscribe(sub_id).await {
            tracing::warn!(pair = %pair_id, sub = %sub_id, error = %e, "scheduler: graph unsubscribe failed");
        }
    }
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

    // Emit a per-pair audit event for each symlink skipped during this cycle's scan.
    // Per the 01-domain-model decision: scanner skips with a warning audit event.
    emit_symlink_skips(inputs, &pair).await;

    Ok(())
}

/// Walk the pair root, emit one `local.symlink.skipped` audit event per encountered symlink.
async fn emit_symlink_skips(inputs: &SchedulerInputs, pair: &onesync_protocol::pair::Pair) {
    use onesync_core::ports::IdGenerator;
    use onesync_protocol::{audit::AuditEvent, enums::AuditLevel, id::AuditTag};

    let scan = match inputs.local_fs.scan(&pair.local_path).await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(pair = %pair.id, error = %e, "scheduler: scan for symlinks failed");
            return;
        }
    };
    for symlink in scan.symlinks_skipped {
        let evt = AuditEvent {
            id: inputs.ids.new_id::<AuditTag>(),
            ts: inputs.clock.now(),
            level: AuditLevel::Warn,
            kind: "local.symlink.skipped".to_owned(),
            pair_id: Some(pair.id),
            payload: {
                let mut m = serde_json::Map::new();
                m.insert(
                    "path".to_owned(),
                    serde_json::Value::String(symlink.display().to_string()),
                );
                m
            },
        };
        inputs.audit.emit(evt);
    }
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
