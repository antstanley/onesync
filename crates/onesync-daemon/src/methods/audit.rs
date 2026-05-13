//! `audit.*` method handlers.
//!
//! - `audit.tail` — live streaming of audit events over the per-connection writer
//! - `audit.search` — query historical audit events with optional filters

use onesync_protocol::{enums::AuditLevel, id::PairId, primitives::Timestamp};
use serde::Deserialize;
use serde_json::Value;

use super::{ConnCtx, MethodError};

/// Maximum number of rows returned in a single search response. Bounds memory and protects
/// the IPC framing limit when a caller requests an unreasonably large window.
const MAX_AUDIT_LIMIT: usize = 1_000;

/// `audit.tail` — register a subscription for live audit events.
///
/// Returns `{ subscription_id }`. The daemon's `DaemonAuditSink` fans new events to every
/// registered subscriber as `audit.event` notifications. This handler spawns a forwarder
/// task that drains the subscription's mpsc receiver into the connection's outbound
/// notification channel, where the per-connection writer task serialises them onto the
/// IPC socket. The forwarder exits when either side closes its channel.
pub async fn tail(ctx: &ConnCtx, _params: &Value) -> Result<Value, MethodError> {
    use crate::ipc::subscriptions::SubscriptionId;
    use onesync_core::ports::IdGenerator;
    use onesync_protocol::id::AuditTag;

    let id_str = ctx.ids.new_id::<AuditTag>().to_string();
    let sub_id = SubscriptionId::new(format!("sub-tail-{id_str}"));
    let mut sub_rx = ctx.subscriptions.insert(sub_id.clone());
    let notif_tx = ctx.notif_tx.clone();
    tokio::spawn(async move {
        while let Some(notif) = sub_rx.recv().await {
            if notif_tx.send(notif).await.is_err() {
                break;
            }
        }
    });
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
pub async fn search(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
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
