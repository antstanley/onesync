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

/// Graph caps `/subscriptions` expiration at 3 days. We mirror that ceiling when
/// initially registering and on every renewal.
const SUBSCRIPTION_TTL_DAYS: i64 = 3;
/// Renewal check interval. Once per hour is plenty: the renewal threshold is far larger.
const SUBSCRIPTION_RENEW_TICK_S: u64 = 60 * 60;
/// Renew a subscription when it is within this many seconds of expiry.
const SUBSCRIPTION_RENEW_LEAD_S: i64 = 60 * 60 * 12; // 12 hours

/// One entry in the scheduler's in-process subscription registry.
struct SubscriptionInfo {
    sub_id: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

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
        // Registered subscriptions keyed by pair id, so we can renew + unsubscribe.
        let mut subscriptions: std::collections::HashMap<PairId, SubscriptionInfo> =
            std::collections::HashMap::new();

        if let Some(url) = webhook_cfg.notification_url.as_deref() {
            register_initial_subscriptions(&inputs, url, &mut subscriptions).await;
        }

        let mut tick = tokio::time::interval(Duration::from_millis(DELTA_POLL_INTERVAL_MS));
        // First tick fires immediately; eat it so we do not race startup with `pair.add`.
        tick.tick().await;

        let mut renew_tick = tokio::time::interval(Duration::from_secs(SUBSCRIPTION_RENEW_TICK_S));
        renew_tick.tick().await; // skip the immediate first fire

        loop {
            tokio::select! {
                _ = tick.tick() => {
                    run_due_pairs(&inputs).await;
                }
                _ = renew_tick.tick() => {
                    renew_due_subscriptions(&inputs, &mut subscriptions).await;
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
    out: &mut std::collections::HashMap<PairId, SubscriptionInfo>,
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
        let token_source = VaultBackedTokenSource::from_inputs(inputs, account.id, client_id);
        let remote =
            GraphAdapter::with_client(inputs.http.clone(), token_source, account.drive_id.clone());
        let client_state = pair.id.to_string();
        match remote
            .subscribe(&account.drive_id, notification_url, &client_state)
            .await
        {
            Ok(sub_id) => {
                let expires_at = subscription_expiry_from_now();
                tracing::info!(pair = %pair.id, sub = %sub_id, expires = %expires_at, "scheduler: graph subscription registered");
                out.insert(pair.id, SubscriptionInfo { sub_id, expires_at });
            }
            Err(e) => {
                tracing::warn!(pair = %pair.id, error = %e, "scheduler: graph subscribe failed");
            }
        }
    }
}

/// Renew every tracked subscription whose expiration is within
/// `SUBSCRIPTION_RENEW_LEAD_S`. Failed renewals are logged but do not remove the entry
/// from the registry — the next tick retries.
async fn renew_due_subscriptions(
    inputs: &SchedulerInputs,
    subscriptions: &mut std::collections::HashMap<PairId, SubscriptionInfo>,
) {
    if subscriptions.is_empty() {
        return;
    }
    // LINT: subscription expiry is wall-clock by definition.
    #[allow(clippy::disallowed_methods)]
    let now = chrono::Utc::now();
    let lead = chrono::Duration::seconds(SUBSCRIPTION_RENEW_LEAD_S);
    let due: Vec<PairId> = subscriptions
        .iter()
        .filter(|(_, info)| info.expires_at - now <= lead)
        .map(|(pair_id, _)| *pair_id)
        .collect();
    for pair_id in due {
        let Some(info) = subscriptions.get(&pair_id) else {
            continue;
        };
        let sub_id = info.sub_id.clone();
        let Ok(Some(pair)) = inputs.state.pair_get(&pair_id).await else {
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
        let new_expiry = subscription_expiry_from_now();
        let token_source = VaultBackedTokenSource::from_inputs(inputs, account.id, client_id);
        let remote = GraphAdapter::with_client(inputs.http.clone(), token_source, account.drive_id);
        match remote
            .renew_subscription(&sub_id, &new_expiry.to_rfc3339())
            .await
        {
            Ok(()) => {
                tracing::info!(pair = %pair_id, sub = %sub_id, expires = %new_expiry, "scheduler: graph subscription renewed");
                if let Some(entry) = subscriptions.get_mut(&pair_id) {
                    entry.expires_at = new_expiry;
                }
            }
            Err(e) => {
                tracing::warn!(pair = %pair_id, sub = %sub_id, error = %e, "scheduler: graph subscription renewal failed");
            }
        }
    }
}

#[allow(clippy::disallowed_methods)]
// LINT: subscription expiry is wall-clock by definition.
fn subscription_expiry_from_now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + chrono::Duration::days(SUBSCRIPTION_TTL_DAYS)
}

async fn unregister_subscriptions(
    inputs: &SchedulerInputs,
    subscriptions: &std::collections::HashMap<PairId, SubscriptionInfo>,
) {
    for (pair_id, info) in subscriptions {
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
        let token_source = VaultBackedTokenSource::from_inputs(inputs, account.id, client_id);
        let remote = GraphAdapter::with_client(inputs.http.clone(), token_source, account.drive_id);
        if let Err(e) = remote.unsubscribe(&info.sub_id).await {
            tracing::warn!(pair = %pair_id, sub = %info.sub_id, error = %e, "scheduler: graph unsubscribe failed");
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
    let cfg_ref = cfg.as_ref();
    let client_id = cfg_ref
        .map(|c| c.azure_ad_client_id.clone())
        .unwrap_or_default();
    if client_id.is_empty() {
        anyhow::bail!("azure_ad_client_id is unset; refusing to schedule pair {pair_id}");
    }

    // 2a. Metered-network gate. SCNetworkReachability detection is deferred; today the
    // helper is a stub that returns `false`, so the gate is effectively a no-op until a
    // real detector lands. The skip-cycle branch is exercised here so the wiring is
    // correct when it does.
    if let Some(c) = cfg_ref
        && !c.allow_metered
        && is_metered_network()
    {
        tracing::info!(pair = %pair_id, "scheduler: skipping cycle (metered network, allow_metered=false)");
        return Ok(());
    }

    // 2b. Free-space gate. If the local volume drops below `min_free_gib`, mark the pair
    // Errored with reason and skip the cycle. min_free_gib == 0 disables the check.
    if let Some(c) = cfg_ref
        && c.min_free_gib > 0
    {
        match free_space_gib(&pair.local_path) {
            Ok(have) if have < u64::from(c.min_free_gib) => {
                tracing::warn!(
                    pair = %pair_id,
                    have_gib = have,
                    min_gib = c.min_free_gib,
                    "scheduler: free disk below threshold; pausing pair"
                );
                mark_pair_errored(
                    inputs.state.as_ref(),
                    inputs.clock.as_ref(),
                    &pair,
                    "free disk below threshold",
                )
                .await;
                return Ok(());
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(pair = %pair_id, error = %e, "scheduler: free-space probe failed; continuing");
            }
        }
    }

    let notify_enabled = cfg_ref.is_some_and(|c| c.notify);

    // 3. Build a token source backed by the vault + refresh endpoint.
    let token_source = VaultBackedTokenSource::from_inputs(inputs, account.id, client_id);

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

    // Persist the new delta cursor + lifecycle transitions. The cycle itself does not
    // touch the Pair row; the scheduler is the single writer of post-cycle state.
    let promoted_to_active =
        pair.status == PairStatus::Initializing && summary.delta_token.is_some();
    persist_post_cycle(
        inputs.state.as_ref(),
        inputs.clock.as_ref(),
        &pair,
        &summary,
    )
    .await;

    if promoted_to_active && notify_enabled {
        notify_user(
            "onesync",
            &format!("Initial sync complete for {}", pair.display_name),
        );
    }

    // Emit a per-pair audit event for each symlink skipped during this cycle's scan.
    // Per the 01-domain-model decision: scanner skips with a warning audit event.
    emit_symlink_skips(inputs, &pair).await;

    Ok(())
}

/// Apply post-cycle bookkeeping to the pair row: persist the new delta cursor, update
/// `last_sync_at`, and flip `Initializing -> Active` once we have a stable cursor to
/// resume from. A no-op if neither change is needed.
pub(crate) async fn persist_post_cycle(
    state: &dyn StateStore,
    clock: &dyn Clock,
    pair: &onesync_protocol::pair::Pair,
    summary: &onesync_core::engine::CycleSummary,
) {
    let now = clock.now();
    let new_cursor = summary
        .delta_token
        .clone()
        .or_else(|| pair.delta_token.clone());
    let promote_to_active =
        pair.status == PairStatus::Initializing && summary.delta_token.is_some();

    if !promote_to_active && new_cursor == pair.delta_token {
        return;
    }

    let updated = onesync_protocol::pair::Pair {
        status: if promote_to_active {
            PairStatus::Active
        } else {
            pair.status
        },
        delta_token: new_cursor,
        last_sync_at: Some(now),
        updated_at: now,
        ..pair.clone()
    };

    if let Err(e) = state.pair_upsert(&updated).await {
        tracing::warn!(pair = %pair.id, error = %e, "scheduler: pair_upsert post-cycle failed");
    } else if promote_to_active {
        tracing::info!(pair = %pair.id, "scheduler: pair promoted Initializing -> Active");
    }
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
///
/// **Resilience (M12b):** transient errors (Network, Transient) are retried with
/// exponential backoff; `ReAuthRequired` (Microsoft revoked the token) triggers
/// `handle_re_auth_required`, which marks every pair on the account as `Errored` and
/// emits an `account.re_auth_required` audit event before the error propagates.
struct VaultBackedTokenSource {
    http: reqwest::Client,
    vault: Arc<dyn TokenVault>,
    account_id: onesync_protocol::id::AccountId,
    client_id: String,
    cache: tokio::sync::Mutex<Option<CachedToken>>,
    state: Arc<dyn StateStore>,
    audit: Arc<dyn AuditSink>,
    ids: Arc<UlidGenerator>,
    clock: Arc<dyn Clock>,
}

struct CachedToken {
    access_token: String,
    fetched_at: std::time::Instant,
    expires_in: u64,
}

/// Number of refresh attempts before giving up on transient failures (3 attempts ⇒ 2 retries).
const TOKEN_REFRESH_MAX_ATTEMPTS: u32 = 3;
/// Base backoff between transient-failure retries; doubled per attempt (250 ms, 500 ms).
const TOKEN_REFRESH_BACKOFF_BASE_MS: u64 = 250;

impl VaultBackedTokenSource {
    fn from_inputs(
        inputs: &SchedulerInputs,
        account_id: onesync_protocol::id::AccountId,
        client_id: String,
    ) -> Self {
        Self {
            http: inputs.http.clone(),
            vault: inputs.vault.clone(),
            account_id,
            client_id,
            cache: tokio::sync::Mutex::new(None),
            state: inputs.state.clone(),
            audit: inputs.audit.clone(),
            ids: inputs.ids.clone(),
            clock: inputs.clock.clone(),
        }
    }

    /// Side effects performed once we know Microsoft has revoked our refresh token.
    /// Marks every active pair on this account as `Errored` with a `re-auth required`
    /// reason and emits an `account.re_auth_required` audit event.
    async fn handle_re_auth_required(&self) {
        use onesync_core::ports::IdGenerator;
        use onesync_protocol::{
            audit::AuditEvent,
            enums::{AuditLevel, PairStatus},
            id::AuditTag,
        };

        let now = self.clock.now();
        let pairs = match self.state.pairs_list(Some(&self.account_id), false).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(account = %self.account_id, error = %e, "scheduler: pairs_list failed during re-auth handling");
                Vec::new()
            }
        };
        for pair in pairs {
            let updated = onesync_protocol::pair::Pair {
                status: PairStatus::Errored,
                errored_reason: Some("re-auth required".to_owned()),
                updated_at: now,
                ..pair
            };
            if let Err(e) = self.state.pair_upsert(&updated).await {
                tracing::warn!(pair = %updated.id, error = %e, "scheduler: pair_upsert failed during re-auth handling");
            }
        }
        let evt = AuditEvent {
            id: self.ids.new_id::<AuditTag>(),
            ts: now,
            level: AuditLevel::Error,
            kind: "account.re_auth_required".to_owned(),
            pair_id: None,
            payload: {
                let mut m = serde_json::Map::new();
                m.insert(
                    "account_id".to_owned(),
                    serde_json::Value::String(self.account_id.to_string()),
                );
                m
            },
        };
        let _ = self.state.audit_append(&evt).await;
        self.audit.emit(evt);
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
        // Cache miss / expiring soon: refresh, with retry on transient failure.
        let RefreshToken(rt) =
            self.vault
                .load_refresh(&self.account_id)
                .await
                .map_err(|e: VaultError| GraphInternalError::Decode {
                    detail: format!("vault load failed: {e}"),
                })?;

        let tokens = {
            let mut attempt: u32 = 0;
            loop {
                attempt += 1;
                match refresh::refresh(&self.http, "common", &self.client_id, &rt).await {
                    Ok(t) => break t,
                    Err(GraphInternalError::ReAuthRequired { request_id }) => {
                        self.handle_re_auth_required().await;
                        return Err(GraphInternalError::ReAuthRequired { request_id });
                    }
                    Err(
                        e @ (GraphInternalError::Network { .. }
                        | GraphInternalError::Transient { .. }),
                    ) if attempt < TOKEN_REFRESH_MAX_ATTEMPTS => {
                        let delay = std::time::Duration::from_millis(
                            TOKEN_REFRESH_BACKOFF_BASE_MS << (attempt - 1),
                        );
                        tracing::warn!(
                            account = %self.account_id,
                            attempt,
                            ?delay,
                            error = %e,
                            "scheduler: token refresh retrying"
                        );
                        tokio::time::sleep(delay).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        };
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

/// Spawn `osascript` to display a native macOS user notification. Best-effort: failures
/// are logged but do not interrupt the sync cycle. Notifications honour
/// `InstanceConfig.notify`; callers gate the call on that flag.
fn notify_user(title: &str, message: &str) {
    let title = title.to_owned();
    let message = message.to_owned();
    tokio::task::spawn_blocking(move || {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            shell_escape(&message),
            shell_escape(&title)
        );
        if let Err(e) = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
        {
            tracing::debug!(error = %e, "scheduler: osascript notify failed");
        }
    });
}

/// `AppleScript` string literals are double-quoted; backslash and double quote need escaping.
fn shell_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Best-effort metered-network detector. Today this returns `false` unconditionally —
/// the `SCNetworkReachability` + flags inspection is a documented carry-over for the
/// next milestone. Wiring is in place so the gate flips on as soon as the detector lands.
const fn is_metered_network() -> bool {
    false
}

/// Available free-space (GiB) on the volume containing `path`, via `fs2::available_space`.
fn free_space_gib(path: &onesync_protocol::path::AbsPath) -> Result<u64, std::io::Error> {
    const BYTES_PER_GIB: u64 = 1 << 30;
    fs2::available_space(std::path::Path::new(path.as_str())).map(|bytes| bytes / BYTES_PER_GIB)
}

/// Helper used by the free-space / metered gates to record a pair as Errored before
/// returning from `run_one_pair_inner`. Audit + notification are caller-driven.
async fn mark_pair_errored(
    state: &dyn StateStore,
    clock: &dyn Clock,
    pair: &onesync_protocol::pair::Pair,
    reason: &str,
) {
    let now = clock.now();
    let updated = onesync_protocol::pair::Pair {
        status: PairStatus::Errored,
        errored_reason: Some(reason.to_owned()),
        updated_at: now,
        ..pair.clone()
    };
    if let Err(e) = state.pair_upsert(&updated).await {
        tracing::warn!(pair = %pair.id, error = %e, "scheduler: pair_upsert during gate failed");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use super::*;
    use onesync_core::engine::CycleSummary;
    use onesync_core::ports::IdGenerator;
    use onesync_protocol::{
        id::{AccountId, AccountTag, PairTag},
        path::AbsPath,
        primitives::{DeltaCursor, DriveItemId, Timestamp},
    };
    use onesync_state::fakes::InMemoryStore;

    struct FixedClock(Timestamp);
    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    fn pair_id() -> PairId {
        UlidGenerator::default().new_id::<PairTag>()
    }
    fn account_id() -> AccountId {
        UlidGenerator::default().new_id::<AccountTag>()
    }
    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_datetime(chrono::DateTime::from_timestamp(secs, 0).unwrap())
    }

    fn initializing_pair() -> onesync_protocol::pair::Pair {
        onesync_protocol::pair::Pair {
            id: pair_id(),
            account_id: account_id(),
            local_path: "/tmp/m12b-pair".parse::<AbsPath>().unwrap(),
            remote_item_id: DriveItemId::new("root"),
            remote_path: "/".to_owned(),
            display_name: "test".to_owned(),
            status: PairStatus::Initializing,
            paused: false,
            delta_token: None,
            errored_reason: None,
            created_at: ts(0),
            updated_at: ts(0),
            last_sync_at: None,
            conflict_count: 0,
            webhook_enabled: false,
        }
    }

    #[tokio::test]
    async fn persist_post_cycle_promotes_initializing_to_active_when_cursor_returned() {
        let state = InMemoryStore::new();
        let clock = FixedClock(ts(123));
        let pair = initializing_pair();
        state.pair_upsert(&pair).await.unwrap();

        let summary = CycleSummary {
            delta_token: Some(DeltaCursor::new("c-1")),
            ..CycleSummary::default()
        };
        persist_post_cycle(&state, &clock, &pair, &summary).await;

        let after = state.pair_get(&pair.id).await.unwrap().expect("pair");
        assert_eq!(after.status, PairStatus::Active);
        assert_eq!(
            after.delta_token.as_ref().map(DeltaCursor::as_str),
            Some("c-1")
        );
        assert_eq!(after.last_sync_at, Some(ts(123)));
    }

    #[tokio::test]
    async fn persist_post_cycle_leaves_initializing_when_no_cursor_returned() {
        let state = InMemoryStore::new();
        let clock = FixedClock(ts(123));
        let pair = initializing_pair();
        state.pair_upsert(&pair).await.unwrap();

        // No cursor means the delta call did not complete or the fake omitted one;
        // we must not declare success.
        let summary = CycleSummary::default();
        persist_post_cycle(&state, &clock, &pair, &summary).await;

        let after = state.pair_get(&pair.id).await.unwrap().expect("pair");
        assert_eq!(after.status, PairStatus::Initializing);
        assert!(after.delta_token.is_none());
    }

    /// Audit sink that stashes every event for assertion.
    #[derive(Default)]
    struct CapturingAuditSink {
        events: std::sync::Mutex<Vec<onesync_protocol::audit::AuditEvent>>,
    }
    impl onesync_core::ports::AuditSink for CapturingAuditSink {
        fn emit(&self, event: onesync_protocol::audit::AuditEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn account(id: AccountId) -> onesync_protocol::account::Account {
        onesync_protocol::account::Account {
            id,
            kind: onesync_protocol::enums::AccountKind::Personal,
            upn: "user@example.com".to_owned(),
            tenant_id: "9188040d-6c67-4c5b-b112-36a304b66dad".to_owned(),
            drive_id: onesync_protocol::primitives::DriveId::new("drive"),
            display_name: "Test User".to_owned(),
            keychain_ref: onesync_protocol::primitives::KeychainRef::new("ref"),
            scopes: vec!["Files.ReadWrite".to_owned()],
            created_at: ts(0),
            updated_at: ts(0),
        }
    }

    fn pair_on_account(account_id: AccountId, local: &str) -> onesync_protocol::pair::Pair {
        onesync_protocol::pair::Pair {
            id: pair_id(),
            account_id,
            local_path: local.parse::<AbsPath>().unwrap(),
            remote_item_id: DriveItemId::new("r"),
            remote_path: "/r".to_owned(),
            display_name: "p".to_owned(),
            status: PairStatus::Active,
            paused: false,
            delta_token: None,
            errored_reason: None,
            created_at: ts(0),
            updated_at: ts(0),
            last_sync_at: None,
            conflict_count: 0,
            webhook_enabled: false,
        }
    }

    #[tokio::test]
    async fn mark_pair_errored_sets_status_and_reason() {
        let state: Arc<dyn StateStore> = Arc::new(InMemoryStore::new());
        let clock: Arc<dyn Clock> = Arc::new(FixedClock(ts(555)));
        let acct_id = account_id();
        state.account_upsert(&account(acct_id)).await.unwrap();
        let pair = pair_on_account(acct_id, "/tmp/free-low");
        state.pair_upsert(&pair).await.unwrap();

        mark_pair_errored(
            state.as_ref(),
            clock.as_ref(),
            &pair,
            "free disk below threshold",
        )
        .await;

        let after = state.pair_get(&pair.id).await.unwrap().expect("pair");
        assert_eq!(after.status, PairStatus::Errored);
        assert_eq!(
            after.errored_reason.as_deref(),
            Some("free disk below threshold")
        );
    }

    #[test]
    fn shell_escape_protects_quotes_and_backslashes() {
        assert_eq!(
            shell_escape(r#"hello "world" \ backslash"#),
            r#"hello \"world\" \\ backslash"#
        );
    }

    #[test]
    fn is_metered_network_returns_false_today() {
        // Documented stub: stays false until the SCNetworkReachability detector lands.
        assert!(!is_metered_network());
    }

    #[tokio::test]
    async fn handle_re_auth_required_marks_pairs_errored_and_emits_audit() {
        let state: Arc<dyn StateStore> = Arc::new(InMemoryStore::new());
        let audit_capture = Arc::new(CapturingAuditSink::default());
        let audit: Arc<dyn AuditSink> = audit_capture.clone();
        let ids = Arc::new(UlidGenerator::default());
        let clock: Arc<dyn Clock> = Arc::new(FixedClock(ts(999)));
        let acct_id = account_id();

        state.account_upsert(&account(acct_id)).await.unwrap();
        let p1 = pair_on_account(acct_id, "/tmp/p1");
        let p2 = pair_on_account(acct_id, "/tmp/p2");
        state.pair_upsert(&p1).await.unwrap();
        state.pair_upsert(&p2).await.unwrap();

        let source = VaultBackedTokenSource {
            http: reqwest::Client::new(),
            vault: Arc::new(onesync_keychain::fakes::InMemoryTokenVault::default()),
            account_id: acct_id,
            client_id: "cid".to_owned(),
            cache: tokio::sync::Mutex::new(None),
            state: state.clone(),
            audit,
            ids,
            clock,
        };
        source.handle_re_auth_required().await;

        let after1 = state.pair_get(&p1.id).await.unwrap().unwrap();
        let after2 = state.pair_get(&p2.id).await.unwrap().unwrap();
        assert_eq!(after1.status, PairStatus::Errored);
        assert_eq!(after2.status, PairStatus::Errored);
        assert_eq!(after1.errored_reason.as_deref(), Some("re-auth required"));
        assert_eq!(after2.errored_reason.as_deref(), Some("re-auth required"));

        let events: Vec<_> = audit_capture.events.lock().unwrap().clone();
        assert_eq!(events.len(), 1, "exactly one audit event");
        assert_eq!(events[0].kind, "account.re_auth_required");
        assert_eq!(events[0].level, onesync_protocol::enums::AuditLevel::Error);
        assert_eq!(
            events[0].payload.get("account_id").and_then(|v| v.as_str()),
            Some(acct_id.to_string().as_str())
        );
    }

    #[tokio::test]
    async fn persist_post_cycle_does_not_demote_already_active_pair() {
        let state = InMemoryStore::new();
        let clock = FixedClock(ts(123));
        let mut pair = initializing_pair();
        pair.status = PairStatus::Active;
        pair.delta_token = Some(DeltaCursor::new("c-prev"));
        state.pair_upsert(&pair).await.unwrap();

        let summary = CycleSummary {
            delta_token: Some(DeltaCursor::new("c-next")),
            ..CycleSummary::default()
        };
        persist_post_cycle(&state, &clock, &pair, &summary).await;

        let after = state.pair_get(&pair.id).await.unwrap().expect("pair");
        assert_eq!(after.status, PairStatus::Active);
        assert_eq!(
            after.delta_token.as_ref().map(DeltaCursor::as_str),
            Some("c-next")
        );
    }
}
