//! `service.*` and `subscription.*` method handlers.
//!
//! - `service.shutdown` — gracefully stop the daemon
//! - `service.upgrade.prepare` — drain connections in preparation for upgrade
//! - `service.upgrade.commit` — commit the upgrade (exec new binary)
//! - `subscription.cancel` — cancel a live subscription

use serde_json::Value;

use super::{DispatchCtx, MethodError};

/// `service.shutdown`
pub async fn shutdown(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("service.shutdown"))
}

/// `service.upgrade.prepare`
pub async fn upgrade_prepare(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("service.upgrade.prepare"))
}

/// `service.upgrade.commit`
pub async fn upgrade_commit(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("service.upgrade.commit"))
}

/// `subscription.cancel`
pub async fn subscription_cancel(
    _ctx: &DispatchCtx,
    _params: &Value,
) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("subscription.cancel"))
}
