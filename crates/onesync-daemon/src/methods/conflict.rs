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

/// `conflict.subscribe` — wired alongside the broader subscription layer.
pub async fn subscribe(_ctx: &ConnCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("conflict.subscribe"))
}
