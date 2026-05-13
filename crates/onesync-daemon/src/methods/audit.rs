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

/// `audit.tail` — register a subscription for live audit events.
///
/// Returns `{ subscription_id }`. The daemon's `DaemonAuditSink` fans new events out to every
/// registered subscriber as `audit.event` notifications. The client must read the IPC socket
/// after sending the request to receive the streamed frames.
pub async fn tail(ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    use crate::ipc::subscriptions::SubscriptionId;
    use onesync_core::ports::IdGenerator;
    use onesync_protocol::id::AuditTag;

    let id_str = ctx.ids.new_id::<AuditTag>().to_string();
    let sub_id = SubscriptionId::new(format!("sub-tail-{id_str}"));
    // `insert` returns the receiver; we drop it here because per-connection streaming is
    // delivered via the connection task in `ipc::server`. The receiver-side delivery wiring
    // lands in M10b (subscription streaming end-to-end).
    let _rx = ctx.subscriptions.insert(sub_id.clone());
    Ok(serde_json::json!({ "subscription_id": sub_id.to_string() }))
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
