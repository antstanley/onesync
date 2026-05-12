//! `config.*` method handlers.
//!
//! - `config.get` — return the current `InstanceConfig`
//! - `config.set` — update configuration
//! - `config.reload` — reload configuration from the database (re-reads from store)

use onesync_protocol::{config::InstanceConfig, enums::LogLevel};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{DispatchCtx, MethodError};

/// `config.get` — fetch the singleton instance config.
pub async fn get(ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    let cfg = ctx
        .state
        .config_get()
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    Ok(cfg.map_or(Value::Null, |c| {
        serde_json::to_value(c).unwrap_or(Value::Null)
    }))
}

/// Mutation payload for `config.set`. Each field is optional; provided fields overwrite.
#[derive(Debug, Default, Deserialize)]
struct ConfigSetParams {
    log_level: Option<LogLevel>,
    notify: Option<bool>,
    allow_metered: Option<bool>,
    min_free_gib: Option<u32>,
}

/// `config.set` — update fields of the singleton instance config.
pub async fn set(ctx: &DispatchCtx, params: &Value) -> Result<Value, MethodError> {
    let p: ConfigSetParams = if params.is_null() {
        ConfigSetParams::default()
    } else {
        serde_json::from_value(params.clone()).map_err(|e| {
            MethodError::new(
                onesync_protocol::rpc::INVALID_PARAMS,
                format!("invalid params: {e}"),
            )
        })?
    };

    let now = ctx.clock.now();
    let existing = ctx
        .state
        .config_get()
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;
    let cfg = match existing {
        Some(mut c) => {
            if let Some(v) = p.log_level {
                c.log_level = v;
            }
            if let Some(v) = p.notify {
                c.notify = v;
            }
            if let Some(v) = p.allow_metered {
                c.allow_metered = v;
            }
            if let Some(v) = p.min_free_gib {
                c.min_free_gib = v;
            }
            c.updated_at = now;
            c
        }
        None => InstanceConfig {
            log_level: p.log_level.unwrap_or(LogLevel::Info),
            notify: p.notify.unwrap_or(true),
            allow_metered: p.allow_metered.unwrap_or(false),
            min_free_gib: p.min_free_gib.unwrap_or(2),
            updated_at: now,
        },
    };

    ctx.state
        .config_upsert(&cfg)
        .await
        .map_err(|e| MethodError::new(onesync_protocol::rpc::INTERNAL_ERROR, e.to_string()))?;

    Ok(json!({ "ok": true, "config": cfg }))
}

/// `config.reload` — re-read the config row.
///
/// Onesync keeps the canonical config in the state store, so reloading is equivalent to a fresh
/// `config.get` from the perspective of an external caller.
pub async fn reload(ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    get(ctx, &Value::Null).await
}
