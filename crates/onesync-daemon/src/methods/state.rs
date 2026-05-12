//! `state.*` method handlers.
//!
//! - `state.backup` — create a database backup
//! - `state.export` — export all state as JSON
//! - `state.repair.permissions` — fix filesystem permission issues
//! - `state.compact.now` — run `SQLite` `VACUUM`

use serde_json::Value;

use super::{DispatchCtx, MethodError};

/// `state.backup`
pub fn backup(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("state.backup"))
}

/// `state.export`
pub fn export(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("state.export"))
}

/// `state.repair.permissions`
pub fn repair_permissions(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("state.repair.permissions"))
}

/// `state.compact.now`
pub fn compact_now(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("state.compact.now"))
}
