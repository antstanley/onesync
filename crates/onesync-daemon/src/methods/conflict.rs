//! `conflict.*` method handlers.
//!
//! - `conflict.list` — list unresolved conflicts for a pair
//! - `conflict.get` — fetch one conflict by id
//! - `conflict.resolve` — mark a conflict resolved
//! - `conflict.subscribe` — wired alongside the broader subscription layer

use onesync_protocol::{
    enums::ConflictResolution,
    id::{ConflictId, PairId},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ConnCtx, MethodError};

#[derive(Debug, Deserialize)]
struct ConflictListParams {
    pair: PairId,
}

/// `conflict.list` — unresolved conflicts for a given pair.
pub async fn list(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    let p: ConflictListParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    let conflicts = ctx
        .state
        .conflicts_unresolved(&p.pair)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(serde_json::to_value(conflicts).unwrap_or(Value::Null))
}

#[derive(Debug, Deserialize)]
struct ConflictByIdParams {
    id: ConflictId,
}

/// `conflict.get` — fetch one conflict by id.
pub async fn get(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    let p: ConflictByIdParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    let conflict = ctx
        .state
        .conflict_get(&p.id)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    match conflict {
        Some(c) => Ok(serde_json::to_value(c).unwrap_or(Value::Null)),
        None => Err(MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 1,
            format!("conflict not found: {}", p.id),
        )),
    }
}

#[derive(Debug, Deserialize)]
struct ConflictResolveParams {
    id: ConflictId,
    resolution: ConflictResolution,
    #[serde(default)]
    note: Option<String>,
}

/// `conflict.resolve` — record a resolution for an open conflict.
pub async fn resolve(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    let p: ConflictResolveParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    let now = ctx.clock.now();
    ctx.state
        .conflict_resolve(&p.id, p.resolution, now, p.note)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(json!({ "ok": true, "id": p.id.to_string() }))
}

#[derive(Debug, Default, Deserialize)]
struct ConflictSubscribeParams {
    /// Optional: restrict the stream to conflicts on a single pair.
    #[serde(default)]
    pair: Option<PairId>,
}

/// `conflict.subscribe` — register a subscription for conflict-related notifications.
///
/// Today the stream surfaces audit events whose `kind` starts with `local.case_collision`
/// or `conflict.`; future producer-side notifications can land alongside without changing
/// the handler. `pair` narrows the filter to one pair.
pub async fn subscribe(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    use crate::ipc::subscriptions::SubscriptionId;
    use onesync_core::ports::IdGenerator;
    use onesync_protocol::id::AuditTag;

    let p: ConflictSubscribeParams = if params.is_null() {
        ConflictSubscribeParams::default()
    } else {
        serde_json::from_value(params.clone()).map_err(|e| {
            MethodError::new(
                onesync_protocol::rpc::INVALID_PARAMS,
                format!("invalid params: {e}"),
            )
        })?
    };

    let id_str = ctx.ids.new_id::<AuditTag>().to_string();
    let sub_id = SubscriptionId::new(format!("sub-conflict-{id_str}"));
    let mut sub_rx = ctx.subscriptions.insert(sub_id.clone());
    let notif_tx = ctx.notif_tx.clone();
    let want_pair = p.pair.map(|id| id.to_string());
    tokio::spawn(async move {
        while let Some(notif) = sub_rx.recv().await {
            if !notification_is_conflict_related(&notif) {
                continue;
            }
            if let Some(want) = want_pair.as_deref()
                && notif.params.get("pair_id").and_then(|v| v.as_str()) != Some(want)
            {
                continue;
            }
            if notif_tx.send(notif).await.is_err() {
                break;
            }
        }
    });
    Ok(json!({ "subscription_id": sub_id.to_string() }))
}

fn notification_is_conflict_related(notif: &onesync_protocol::rpc::JsonRpcNotification) -> bool {
    if notif.method.starts_with("conflict.") {
        return true;
    }
    // `audit.event` wraps an `AuditEvent` whose `kind` is the actual event name.
    if notif.method == "audit.event"
        && let Some(kind) = notif.params.get("kind").and_then(|v| v.as_str())
    {
        return kind.starts_with("conflict.") || kind.starts_with("local.case_collision");
    }
    false
}
