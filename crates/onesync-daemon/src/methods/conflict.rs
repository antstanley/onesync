//! `conflict.*` method handlers.
//!
//! - `conflict.list` — list unresolved conflicts
//! - `conflict.get` — fetch one conflict by id
//! - `conflict.resolve` — mark a conflict as resolved
//! - `conflict.subscribe` — subscribe to conflict events (Task 14)

use serde_json::Value;

use super::{DispatchCtx, MethodError};

/// `conflict.list`
pub fn list(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("conflict.list"))
}

/// `conflict.get`
pub fn get(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("conflict.get"))
}

/// `conflict.resolve`
pub fn resolve(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("conflict.resolve"))
}

/// `conflict.subscribe` — subscription wired in Task 14.
pub fn subscribe(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("conflict.subscribe"))
}
