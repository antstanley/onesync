//! `run.*` method handlers.
//!
//! - `run.list` — list recent sync runs
//! - `run.get` — fetch one run by id

use serde_json::Value;

use super::{DispatchCtx, MethodError};

/// `run.list`
pub fn list(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("run.list"))
}

/// `run.get`
pub fn get(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("run.get"))
}
