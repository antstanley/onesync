//! `pair.*` method handlers.
//!
//! - `pair.add` — add a new sync pair
//! - `pair.list` — return all active pairs
//! - `pair.get` — fetch one pair by id
//! - `pair.pause` — pause syncing for a pair
//! - `pair.resume` — resume syncing for a pair
//! - `pair.remove` — delete a pair
//! - `pair.force_sync` — trigger an immediate sync cycle
//! - `pair.status` — return detailed pair status
//! - `pair.subscribe` — subscribe to progress events for a pair (Task 14)

use serde_json::Value;

use super::{DispatchCtx, MethodError};

/// `pair.add`
pub fn add(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.add"))
}

/// `pair.list`
pub fn list(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.list"))
}

/// `pair.get`
pub fn get(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.get"))
}

/// `pair.pause`
pub fn pause(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.pause"))
}

/// `pair.resume`
pub fn resume(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.resume"))
}

/// `pair.remove`
pub fn remove(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.remove"))
}

/// `pair.force_sync`
pub fn force_sync(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.force_sync"))
}

/// `pair.status`
pub fn status(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.status"))
}

/// `pair.subscribe` — subscription wired in Task 14.
pub fn subscribe(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("pair.subscribe"))
}
