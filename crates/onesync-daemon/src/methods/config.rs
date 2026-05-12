//! `config.*` method handlers.
//!
//! - `config.get` — return the current `InstanceConfig`
//! - `config.set` — update configuration
//! - `config.reload` — reload configuration from the database

use serde_json::Value;

use super::{DispatchCtx, MethodError};

/// `config.get`
pub fn get(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("config.get"))
}

/// `config.set`
pub fn set(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("config.set"))
}

/// `config.reload`
pub fn reload(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("config.reload"))
}
