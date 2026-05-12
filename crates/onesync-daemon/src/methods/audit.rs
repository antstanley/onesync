//! `audit.*` method handlers.
//!
//! - `audit.tail` — deferred (live streaming wired alongside the subscription layer)
//! - `audit.search` — query historical audit events with optional filters

use onesync_protocol::{enums::AuditLevel, id::PairId, primitives::Timestamp};
use serde::Deserialize;
use serde_json::Value;

use super::{DispatchCtx, MethodError};

/// Maximum number of rows returned in a single search response. Bounds memory and protects
/// the IPC framing limit when a caller requests an unreasonably large window.
const MAX_AUDIT_LIMIT: usize = 1_000;

/// `audit.tail` — deferred. See module-level note.
pub async fn tail(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("audit.tail"))
}

#[derive(Debug, Deserialize)]
struct AuditSearchParams {
    from: Timestamp,
    to: Timestamp,
    #[serde(default)]
    level: Option<AuditLevel>,
    #[serde(default)]
    pair: Option<PairId>,
    #[serde(default)]
    limit: Option<usize>,
}

/// `audit.search` — search historical audit log within `[from, to]`.
pub async fn search(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    let p: AuditSearchParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    let limit = p.limit.unwrap_or(100).min(MAX_AUDIT_LIMIT);
    let events = ctx
        .state
        .audit_search(&p.from, &p.to, p.level, p.pair.as_ref(), limit)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(serde_json::to_value(events).unwrap_or(Value::Null))
}
