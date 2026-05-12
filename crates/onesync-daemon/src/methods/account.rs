//! `account.*` method handlers.
//!
//! - `account.login.begin` — start the OAuth PKCE flow
//! - `account.login.await` — poll for the OAuth redirect
//! - `account.list` — return all linked accounts
//! - `account.get` — fetch one account by id
//! - `account.remove` — unlink an account

use serde_json::Value;

use super::{DispatchCtx, MethodError};

/// `account.login.begin` — start the OAuth PKCE flow.
pub fn login_begin(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("account.login.begin"))
}

/// `account.login.await` — poll for the OAuth redirect completion.
pub fn login_await(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("account.login.await"))
}

/// `account.list` — return all linked accounts.
pub fn list(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("account.list"))
}

/// `account.get` — fetch one account by id.
pub fn get(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("account.get"))
}

/// `account.remove` — unlink an account.
pub fn remove(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("account.remove"))
}
