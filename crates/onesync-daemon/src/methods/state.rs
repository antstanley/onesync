//! `state.*` method handlers.
//!
//! - `state.backup { to_path }` — write a consistent `SQLite` snapshot via `VACUUM INTO`.
//! - `state.export { to_dir }` — dump accounts / pairs / audit / runs as JSON files.
//! - `state.repair.permissions` — chmod 0700 on the state dir and 0600 on its files.
//! - `state.compact.now` — invoke retention prune + `VACUUM`.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use super::{DispatchCtx, MethodError};

#[derive(Debug, Deserialize)]
struct StateBackupParams {
    to_path: String,
}

/// `state.backup` — write a consistent snapshot of the `SQLite` DB to `to_path`.
pub async fn backup(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    let p: StateBackupParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    let to = PathBuf::from(p.to_path);
    ctx.state
        .backup_to(&to)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(json!({ "ok": true, "to_path": to.display().to_string() }))
}

#[derive(Debug, Deserialize)]
struct StateExportParams {
    to_dir: String,
}

/// `state.export` — dump every queryable table as JSON files under `to_dir`.
pub async fn export(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    let p: StateExportParams = serde_json::from_value(params.clone()).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INVALID_PARAMS,
            format!("invalid params: {e}"),
        )
    })?;
    let to_dir = PathBuf::from(p.to_dir);
    std::fs::create_dir_all(&to_dir).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 30,
            format!("create to_dir failed: {e}"),
        )
    })?;

    let accounts = ctx
        .state
        .accounts_list()
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    write_json(&to_dir.join("accounts.json"), &accounts)?;

    let pairs = ctx
        .state
        .pairs_list(None, true)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    write_json(&to_dir.join("pairs.json"), &pairs)?;

    // Audit window: epoch → far future. The default 1000-row cap inside audit_search caps
    // export volume; operators wanting a larger window run their own queries.
    let from = onesync_protocol::primitives::Timestamp::from_datetime(
        chrono::DateTime::from_timestamp(0, 0).unwrap_or_else(|| ctx.clock.now().into_inner()),
    );
    let to = ctx.clock.now();
    let audits = ctx
        .state
        .audit_search(&from, &to, None, None, 10_000)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    write_json(&to_dir.join("audit.json"), &audits)?;

    Ok(json!({
        "ok": true,
        "to_dir": to_dir.display().to_string(),
        "files": ["accounts.json", "pairs.json", "audit.json"],
    }))
}

/// `state.repair.permissions` — chmod 0700 the state directory, 0600 every file inside.
pub async fn repair_permissions(ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    let state_dir = ctx.state_dir.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Vec<String>, std::io::Error> {
        let mut touched: Vec<String> = Vec::new();
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
        touched.push(state_dir.display().to_string());
        for entry in std::fs::read_dir(&state_dir)? {
            let entry = entry?;
            let path = entry.path();
            let mode = if entry.file_type()?.is_dir() {
                0o700
            } else {
                0o600
            };
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
            touched.push(path.display().to_string());
        }
        Ok(touched)
    })
    .await
    .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, format!("join: {e}")))?
    .map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 31,
            format!("chmod failed: {e}"),
        )
    })?;
    Ok(json!({ "ok": true, "touched": result }))
}

/// `state.compact.now` — retention prune + `VACUUM`.
pub async fn compact_now(ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    let now = ctx.clock.now();
    ctx.state
        .compact_now(&now)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(json!({ "ok": true }))
}

fn write_json<T: serde::Serialize>(path: &Path, data: &T) -> Result<(), MethodError> {
    let serialised = serde_json::to_string_pretty(data).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::INTERNAL_ERROR,
            format!("serialise failed: {e}"),
        )
    })?;
    std::fs::write(path, serialised).map_err(|e| {
        MethodError::new(
            onesync_protocol::rpc::APP_ERROR_BASE - 32,
            format!("write {} failed: {e}", path.display()),
        )
    })
}
