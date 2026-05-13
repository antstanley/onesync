//! `run.*` method handlers.
//!
//! - `run.list` — list recent sync runs for a pair
//! - `run.get` — fetch one run by id

use onesync_protocol::id::{PairId, SyncRunId};
use serde::Deserialize;
use serde_json::Value;

use super::{ConnCtx, MethodError};

/// Maximum runs returned by one `run.list` call. Most consumers want a recent slice; deeper
/// inspection is via `audit.search`.
const MAX_RUN_LIMIT: usize = 200;

#[derive(Debug, Deserialize)]
struct RunListParams {
    pair: PairId,
    #[serde(default)]
    limit: Option<usize>,
}

/// `run.list` — most-recent runs for a pair (newest first).
pub async fn list(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    let p: RunListParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    let limit = p.limit.unwrap_or(20).min(MAX_RUN_LIMIT);
    let runs = ctx
        .state
        .runs_recent(&p.pair, limit)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(serde_json::to_value(runs).unwrap_or(Value::Null))
}

#[derive(Debug, Deserialize)]
struct RunByIdParams {
    id: SyncRunId,
}

/// `run.get` — fetch one run by id.
pub async fn get(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    let p: RunByIdParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    let run = ctx
        .state
        .run_get(&p.id)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    match run {
        Some(r) => Ok(serde_json::to_value(r).unwrap_or(Value::Null)),
        None => Err(MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 1,
            format!("run not found: {}", p.id),
        )),
    }
}
