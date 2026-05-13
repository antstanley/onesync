//! `service.*` and `subscription.*` method handlers.
//!
//! - `service.shutdown` — trigger graceful daemon stop via the dispatch-resident
//!   `ShutdownToken`. The `drain` param is accepted for forward-compatibility but the
//!   current shutdown path always drains.
//! - `service.upgrade.prepare` / `service.upgrade.commit` — deferred; the upgrade flow
//!   needs binary-swap orchestration that lives in a future milestone (M10).
//! - `subscription.cancel` — deferred until the subscription streaming layer lands (M10).

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ConnCtx, MethodError};

#[derive(Debug, Default, Deserialize)]
struct ServiceShutdownParams {
    /// Reserved for future use: if `false`, exit without waiting for in-flight cycles.
    /// Today the daemon always drains; the field is accepted for forward-compatibility.
    #[serde(default)]
    #[allow(dead_code)]
    drain: Option<bool>,
}

/// `service.shutdown` — trigger the daemon's `ShutdownToken`.
pub async fn shutdown(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    let _p: ServiceShutdownParams = if params.is_null() {
        ServiceShutdownParams::default()
    } else {
        serde_json::from_value(params.clone()).unwrap_or_default()
    };
    ctx.shutdown_token.trigger();
    Ok(json!({ "ok": true }))
}

/// `service.upgrade.prepare`
pub async fn upgrade_prepare(_ctx: &ConnCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("service.upgrade.prepare"))
}

/// `service.upgrade.commit`
pub async fn upgrade_commit(_ctx: &ConnCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("service.upgrade.commit"))
}

/// `subscription.cancel`
pub async fn subscription_cancel(
    _ctx: &ConnCtx,
    _params: &Value,
) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("subscription.cancel"))
}
