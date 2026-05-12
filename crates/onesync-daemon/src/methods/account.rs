//! `account.*` method handlers.
//!
//! - `account.login.begin` / `account.login.await` — deferred (OAuth wiring lands when the Graph
//!   adapter is connected end-to-end).
//! - `account.list` — return all linked accounts
//! - `account.get` — fetch one account by id
//! - `account.remove` — unlink an account (FK-cascades remove its pairs)

use serde::Deserialize;
use serde_json::{Value, json};

use onesync_protocol::id::AccountId;

use super::{DispatchCtx, MethodError};

/// `account.login.begin` — start the OAuth PKCE flow.
///
/// Deferred until the Graph adapter is wired into the daemon. Today the keychain is reachable
/// via the keychain port and the in-crate MSAL helper exists; what's missing is the daemon-owned
/// PKCE state machine and the loopback redirect listener.
pub async fn login_begin(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("account.login.begin"))
}

/// `account.login.await` — poll for OAuth completion. Paired with `login_begin`.
pub async fn login_await(_ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("account.login.await"))
}

/// `account.list` — return all linked accounts.
pub async fn list(ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    let accts = ctx
        .state
        .accounts_list()
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(serde_json::to_value(accts).unwrap_or(Value::Null))
}

#[derive(Debug, Deserialize)]
struct AccountByIdParams {
    id: AccountId,
}

/// `account.get` — fetch one account by id.
pub async fn get(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    let p: AccountByIdParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    let acct = ctx
        .state
        .account_get(&p.id)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    match acct {
        Some(a) => Ok(serde_json::to_value(a).unwrap_or(Value::Null)),
        None => Err(MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 1,
            format!("account not found: {}", p.id),
        )),
    }
}

/// `account.remove` — unlink an account and cascade-remove its pairs.
pub async fn remove(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    let p: AccountByIdParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    ctx.state
        .account_remove(&p.id)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(json!({ "ok": true, "id": p.id.to_string() }))
}
