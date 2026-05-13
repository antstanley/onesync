//! `pair.*` method handlers.
//!
//! All CRUD methods (`add`, `list`, `get`, `pause`, `resume`, `remove`) plus the runtime
//! methods (`force_sync`, `status`) are wired against the engine scheduler and state store.
//! Only `pair.subscribe` remains deferred; it lands with the subscription streaming layer
//! in M10.

use onesync_core::ports::{IdGenerator, RefreshToken};
use onesync_graph::{auth::refresh, items};
use onesync_protocol::{
    audit::AuditEvent,
    enums::PairStatus,
    id::{AccountId, AuditTag, PairId, PairTag},
    pair::Pair,
    path::AbsPath,
    primitives::DriveItemId,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ConnCtx, MethodError};

#[derive(Debug, Deserialize)]
struct PairAddParams {
    account_id: AccountId,
    local_path: String,
    /// Remote path beneath the account's drive root, e.g. `/Documents/OneSyncTest`.
    /// Must already exist (use the `OneDrive` web UI or `mkdir` via Files app to create it).
    remote_path: String,
    #[serde(default)]
    display_name: Option<String>,
}

/// `pair.add` — validate the local path, resolve the remote root, mint a `Pair` row.
#[allow(clippy::too_many_lines)]
// LINT: linear validation + lookup flow; splitting only adds indirection.
pub async fn add(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    let p: PairAddParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;

    // 1. Look up the account (must exist and not be deleted).
    let account = ctx
        .state
        .account_get(&p.account_id)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?
        .ok_or_else(|| {
            MethodError::new(
                onesync_protocol::rpc::APP_ERROR_BASE - 20,
                format!("account not found: {}", p.account_id),
            )
        })?;

    // 2. Parse and create the local path.
    let local_path: AbsPath = p.local_path.parse().map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("local_path is not an absolute POSIX path: {e}"),
        )
    })?;
    ctx.local_fs.mkdir_p(&local_path).await.map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 21,
            format!("local_path is not usable: {e}"),
        )
    })?;

    // 3. Load the refresh token + client id, refresh the access token.
    let cfg = ctx
        .state
        .config_get()
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    let client_id = cfg
        .as_ref()
        .map(|c| c.azure_ad_client_id.clone())
        .unwrap_or_default();
    if client_id.is_empty() {
        return Err(MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 10,
            "azure_ad_client_id is unset",
        ));
    }
    let RefreshToken(refresh_value) = ctx.vault.load_refresh(&account.id).await.map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 22,
            format!("keychain load failed for account {}: {e}", account.id),
        )
    })?;
    let tokens = refresh::refresh(&ctx.http, "common", &client_id, &refresh_value)
        .await
        .map_err(|e| {
            MethodError::new(
                onesync_protocol::rpc::APP_ERROR_BASE - 23,
                format!("token refresh failed: {e}"),
            )
        })?;
    // Persist the rotated refresh token (Microsoft rotates these on every refresh).
    let _ = ctx
        .vault
        .store_refresh(&account.id, &RefreshToken(tokens.refresh_token))
        .await;

    // 4. Resolve the remote folder. Must already exist.
    let remote_item = items::item_by_path(
        &ctx.http,
        &tokens.access_token,
        &account.drive_id,
        &p.remote_path,
    )
    .await
    .map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 24,
            format!("remote_path lookup failed: {e}"),
        )
    })?
    .ok_or_else(|| {
        MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 25,
            format!(
                "remote_path not found in account drive (create it in OneDrive first): {}",
                p.remote_path
            ),
        )
    })?;

    // 5. Build the Pair row.
    let now = ctx.clock.now();
    let pair = Pair {
        id: ctx.ids.new_id::<PairTag>(),
        account_id: account.id,
        local_path,
        remote_item_id: DriveItemId::new(remote_item.id),
        remote_path: p.remote_path,
        display_name: p.display_name.unwrap_or_else(|| remote_item.name.clone()),
        status: PairStatus::Initializing,
        paused: false,
        delta_token: None,
        errored_reason: None,
        created_at: now,
        updated_at: now,
        last_sync_at: None,
        conflict_count: 0,
        webhook_enabled: false,
    };
    ctx.state
        .pair_upsert(&pair)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;

    // 6. Audit.
    let evt = AuditEvent {
        id: ctx.ids.new_id::<AuditTag>(),
        ts: now,
        level: onesync_protocol::enums::AuditLevel::Info,
        kind: "pair.added".to_owned(),
        pair_id: Some(pair.id),
        payload: {
            let mut m = serde_json::Map::new();
            m.insert(
                "account_id".to_owned(),
                Value::String(account.id.to_string()),
            );
            m.insert(
                "local_path".to_owned(),
                Value::String(pair.local_path.as_str().to_owned()),
            );
            m.insert(
                "remote_path".to_owned(),
                Value::String(pair.remote_path.clone()),
            );
            m
        },
    };
    let _ = ctx.state.audit_append(&evt).await;
    ctx.audit.emit(evt);

    Ok(serde_json::to_value(pair).unwrap_or(Value::Null))
}

/// `pair.list` — return all non-removed pairs.
pub async fn list(ctx: &ConnCtx, _params: &Value) -> Result<Value, MethodError> {
    let pairs = ctx
        .state
        .pairs_list(None, false)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(serde_json::to_value(pairs).unwrap_or(Value::Null))
}

#[derive(Debug, Deserialize)]
struct PairByIdParams {
    id: PairId,
}

/// `pair.get` — fetch one pair by id.
pub async fn get(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    let p: PairByIdParams = parse_pair_id(params)?;
    let pair = ctx
        .state
        .pair_get(&p.id)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    match pair {
        Some(pair) => Ok(serde_json::to_value(pair).unwrap_or(Value::Null)),
        None => Err(MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 1,
            format!("pair not found: {}", p.id),
        )),
    }
}

/// `pair.pause` — set status=Paused, paused=true.
pub async fn pause(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    set_paused(ctx, params, true).await
}

/// `pair.resume` — set status=Active, paused=false.
pub async fn resume(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    set_paused(ctx, params, false).await
}

/// `pair.remove` — soft-delete (status=Removed). State stays so audits/runs remain queryable.
pub async fn remove(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    let p: PairByIdParams = parse_pair_id(params)?;
    let mut pair = require_pair(ctx, &p.id).await?;
    pair.status = PairStatus::Removed;
    pair.paused = true;
    pair.updated_at = ctx.clock.now();
    ctx.state
        .pair_upsert(&pair)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(json!({ "ok": true, "id": pair.id.to_string() }))
}

/// `pair.force_sync` — push a manual trigger through the scheduler.
pub async fn force_sync(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    let p: PairByIdParams = parse_pair_id(params)?;
    require_pair(ctx, &p.id).await?;
    ctx.scheduler.force_sync(p.id).await.map_err(|()| {
        MethodError::new(
            onesync_protocol::rpc::INTERNAL_ERROR,
            "scheduler is not accepting new triggers (shutting down?)",
        )
    })?;
    Ok(json!({ "ok": true, "pair": p.id.to_string() }))
}

/// `pair.status` — aggregate Pair + recent runs + unresolved-conflicts count.
pub async fn status(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    let p: PairByIdParams = parse_pair_id(params)?;
    let pair = require_pair(ctx, &p.id).await?;
    let recent_runs = ctx
        .state
        .runs_recent(&pair.id, 5)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    let unresolved = ctx
        .state
        .conflicts_unresolved(&pair.id)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(json!({
        "pair": pair,
        "recent_runs": recent_runs,
        "unresolved_conflicts": unresolved.len(),
    }))
}

/// `pair.subscribe` — wired alongside the broader subscription streaming layer in M10.
pub async fn subscribe(_ctx: &ConnCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.subscribe"))
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn parse_pair_id(params: &Value) -> Result<PairByIdParams, MethodError> {
    serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })
}

async fn require_pair(
    ctx: &ConnCtx,
    id: &PairId,
) -> Result<onesync_protocol::pair::Pair, MethodError> {
    ctx.state
        .pair_get(id)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?
        .ok_or_else(|| {
            MethodError::new(
                onesync_protocol::rpc::APP_ERROR_BASE - 1,
                format!("pair not found: {id}"),
            )
        })
}

async fn set_paused(ctx: &ConnCtx, params: &Value, paused: bool) -> Result<Value, MethodError> {
    let p = parse_pair_id(params)?;
    let mut pair = require_pair(ctx, &p.id).await?;
    pair.paused = paused;
    pair.status = if paused {
        PairStatus::Paused
    } else {
        PairStatus::Active
    };
    pair.updated_at = ctx.clock.now();
    ctx.state
        .pair_upsert(&pair)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(json!({ "ok": true, "id": pair.id.to_string(), "paused": paused }))
}
