//! `pair.*` method handlers.
//!
//! Implemented:
//! - `pair.list`, `pair.get`, `pair.pause`, `pair.resume`, `pair.remove` (soft-delete)
//!
//! Deferred (need engine/graph wiring):
//! - `pair.add` — requires account-side Graph resolution of the remote root.
//! - `pair.force_sync` — schedules an engine cycle; engine integration lands in a later task.
//! - `pair.status` — extends the basic pair row with run history; partial today.
//! - `pair.subscribe` — wired alongside the broader subscription layer.

use onesync_protocol::{enums::PairStatus, id::PairId};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{DispatchCtx, MethodError};

/// `pair.add` — deferred. See module-level note.
pub async fn add(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.add"))
}

/// `pair.list` — return all non-removed pairs.
pub async fn list(ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
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
pub async fn get(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
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
pub async fn pause(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    set_paused(ctx, params, true).await
}

/// `pair.resume` — set status=Active, paused=false.
pub async fn resume(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    set_paused(ctx, params, false).await
}

/// `pair.remove` — soft-delete (status=Removed). State stays so audits/runs remain queryable.
pub async fn remove(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
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

/// `pair.force_sync` — deferred until engine wiring lands.
pub async fn force_sync(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.force_sync"))
}

/// `pair.status` — deferred. Today's wiring lacks the run aggregation hooks the status object
/// promises (delta-cursor age, in-flight ops, pending bytes).
pub async fn status(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.status"))
}

/// `pair.subscribe` — wired alongside the broader subscription layer.
pub async fn subscribe(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
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
    ctx: &DispatchCtx,
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

async fn set_paused(ctx: &DispatchCtx, params: &Value, paused: bool) -> Result<Value, MethodError> {
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
