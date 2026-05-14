//! `service.*` and `subscription.*` method handlers.
//!
//! - `service.shutdown` — trigger graceful daemon stop via the dispatch-resident
//!   `ShutdownToken`. The `drain` param is accepted for forward-compatibility but the
//!   current shutdown path always drains.
//! - `service.upgrade.prepare` / `service.upgrade.commit` — two-phase binary swap.
//!   `prepare` validates a staged binary path and stashes it in [`super::DispatchCtx`];
//!   `commit` triggers the shutdown token, which drains in-flight cycles and
//!   eventually returns control to `main`, which `exec`s the staged binary.
//! - `subscription.cancel` — deferred until the subscription streaming layer lands (M10).

use std::os::unix::fs::PermissionsExt as _;

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

#[derive(Debug, Deserialize)]
struct UpgradePrepareParams {
    /// Absolute path to the staged replacement binary. Must exist and be executable.
    binary_path: String,
}

/// `service.upgrade.prepare` — validate a staged binary and stash its path. No drain.
///
/// Drain happens in `service.upgrade.commit`. Decoupling validation from the
/// shutdown-trigger lets the operator catch path mistakes (missing file, wrong
/// permissions, wrong architecture) without taking the daemon down.
pub async fn upgrade_prepare(ctx: &ConnCtx, params: &Value) -> Result<Value, MethodError> {
    let p: UpgradePrepareParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;

    let path = std::path::PathBuf::from(&p.binary_path);
    if !path.is_absolute() {
        return Err(MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("binary_path must be absolute: {}", p.binary_path),
        ));
    }
    let meta = std::fs::metadata(&path).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 50,
            format!("binary_path does not exist or is not readable: {e}"),
        )
    })?;
    if !meta.is_file() {
        return Err(MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 51,
            format!("binary_path is not a regular file: {}", p.binary_path),
        ));
    }
    if meta.permissions().mode() & 0o111 == 0 {
        return Err(MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 52,
            format!("binary_path is not executable: {}", p.binary_path),
        ));
    }

    {
        let mut guard = ctx
            .upgrade_staging
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(path.clone());
    }

    Ok(json!({
        "ok": true,
        "staged_path": path.display().to_string(),
    }))
}

/// `service.upgrade.commit` — trigger the shutdown token. `main` reads the staged path
/// and `exec`s into the new binary after the IPC server has drained.
pub async fn upgrade_commit(ctx: &ConnCtx, _params: &Value) -> Result<Value, MethodError> {
    let staged = {
        let guard = ctx
            .upgrade_staging
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.clone()
    };
    let Some(path) = staged else {
        return Err(MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 53,
            "no upgrade staged — call service.upgrade.prepare first",
        ));
    };
    ctx.shutdown_token.trigger();
    Ok(json!({
        "ok": true,
        "staged_path": path.display().to_string(),
        "message": "shutdown signalled — daemon will exec staged binary after drain",
    }))
}

/// `subscription.cancel`
pub async fn subscription_cancel(_ctx: &ConnCtx, _params: &Value) -> Result<Value, MethodError> {
    Err(MethodError::not_implemented("subscription.cancel"))
}
