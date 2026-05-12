//! `health.*` method handlers.
//!
//! - `health.ping` → `{ uptime_s, version, schema_version }`
//! - `health.diagnostics` → full [`Diagnostics`] snapshot

use serde_json::Value;

use super::{DispatchCtx, MethodError};

/// Current application version string, taken from Cargo metadata at compile time.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Monotonically increasing database schema version.
///
/// Bumped whenever a new migration is added.
const SCHEMA_VERSION: u32 = 1;

/// Handle `health.ping`.
///
/// Returns `{ "uptime_s": <u64>, "version": "<semver>", "schema_version": <u32> }`.
// LINT: Result return is the uniform handler signature; ping never errs but others do.
#[allow(clippy::unnecessary_wraps)]
pub async fn ping(ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    let uptime_s = ctx.started_at.elapsed().as_secs();
    Ok(serde_json::json!({
        "uptime_s": uptime_s,
        "version": VERSION,
        "schema_version": SCHEMA_VERSION,
    }))
}

/// Handle `health.diagnostics`.
///
/// Returns a full [`onesync_protocol::handles::Diagnostics`] snapshot.
/// Full implementation (loading pairs, accounts, config) lands in Task 13;
/// for now returns a minimal stub.
// LINT: Result return is the uniform handler signature; diagnostics will err on db failures.
#[allow(clippy::unnecessary_wraps)]
pub async fn diagnostics(ctx: &DispatchCtx, _params: &Value) -> Result<Value, MethodError> {
    let uptime_s = ctx.started_at.elapsed().as_secs();
    Ok(serde_json::json!({
        "version": VERSION,
        "schema_version": SCHEMA_VERSION,
        "uptime_s": uptime_s,
        "pairs": [],
        "accounts": [],
        "config": serde_json::Value::Null,
        "subscriptions": 0_u32,
    }))
}
