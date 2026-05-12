//! `audit.*` method handlers.
//!
//! - `audit.tail` — subscribe to a live audit stream (Task 14)
//! - `audit.search` — query historical audit events

use serde_json::Value;

use super::{DispatchCtx, MethodError};

/// `audit.tail` — subscribe to live audit events (wired in Task 14).
pub fn tail(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("audit.tail"))
}

/// `audit.search` — search historical audit log.
pub fn search(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("audit.search"))
}
