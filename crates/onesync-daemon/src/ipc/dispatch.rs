//! JSON-RPC 2.0 dispatch: route a request to the appropriate method handler.
//!
//! [`dispatch`] is the single entry point called for every frame received by
//! the IPC server.  It parses the method name, calls the handler, and wraps
//! the result in a [`JsonRpcResponse`].

use onesync_protocol::rpc::{self, JsonRpcRequest, JsonRpcResponse};

use crate::methods::{self, DispatchCtx};

/// Dispatch one JSON-RPC request to the appropriate handler.
///
/// Returns a [`JsonRpcResponse`] suitable for serialising back to the client.
#[must_use]
pub fn dispatch(req: &JsonRpcRequest, ctx: &DispatchCtx) -> JsonRpcResponse {
    let id = id_str(req.id.as_ref());

    let result = match req.method.as_str() {
        // health
        "health.ping" => methods::health::ping(ctx, &req.params),
        "health.diagnostics" => methods::health::diagnostics(ctx, &req.params),
        // config
        "config.get" => methods::config::get(ctx, &req.params),
        "config.set" => methods::config::set(ctx, &req.params),
        "config.reload" => methods::config::reload(ctx, &req.params),
        // account
        "account.login.begin" => methods::account::login_begin(ctx, &req.params),
        "account.login.await" => methods::account::login_await(ctx, &req.params),
        "account.list" => methods::account::list(ctx, &req.params),
        "account.get" => methods::account::get(ctx, &req.params),
        "account.remove" => methods::account::remove(ctx, &req.params),
        // pair
        "pair.add" => methods::pair::add(ctx, &req.params),
        "pair.list" => methods::pair::list(ctx, &req.params),
        "pair.get" => methods::pair::get(ctx, &req.params),
        "pair.pause" => methods::pair::pause(ctx, &req.params),
        "pair.resume" => methods::pair::resume(ctx, &req.params),
        "pair.remove" => methods::pair::remove(ctx, &req.params),
        "pair.force_sync" => methods::pair::force_sync(ctx, &req.params),
        "pair.status" => methods::pair::status(ctx, &req.params),
        "pair.subscribe" => methods::pair::subscribe(ctx, &req.params),
        // conflict
        "conflict.list" => methods::conflict::list(ctx, &req.params),
        "conflict.get" => methods::conflict::get(ctx, &req.params),
        "conflict.resolve" => methods::conflict::resolve(ctx, &req.params),
        "conflict.subscribe" => methods::conflict::subscribe(ctx, &req.params),
        // audit
        "audit.tail" => methods::audit::tail(ctx, &req.params),
        "audit.search" => methods::audit::search(ctx, &req.params),
        // run
        "run.list" => methods::run::list(ctx, &req.params),
        "run.get" => methods::run::get(ctx, &req.params),
        // state
        "state.backup" => methods::state::backup(ctx, &req.params),
        "state.export" => methods::state::export(ctx, &req.params),
        "state.repair.permissions" => methods::state::repair_permissions(ctx, &req.params),
        "state.compact.now" => methods::state::compact_now(ctx, &req.params),
        // service + subscription
        "service.shutdown" => methods::service::shutdown(ctx, &req.params),
        "service.upgrade.prepare" => methods::service::upgrade_prepare(ctx, &req.params),
        "service.upgrade.commit" => methods::service::upgrade_commit(ctx, &req.params),
        "subscription.cancel" => methods::service::subscription_cancel(ctx, &req.params),
        _ => Err(methods::MethodError::new(
            rpc::METHOD_NOT_FOUND,
            format!("method not found: {}", req.method),
        )),
    };

    match result {
        Ok(value) => JsonRpcResponse::ok(id, value),
        Err(e) => JsonRpcResponse::error(Some(id), e.code, e.message),
    }
}

/// Convert a JSON-RPC request `id` to a `String` for the response.
///
/// Falls back to `"null"` when the id is absent or not a string/number.
fn id_str(id: Option<&serde_json::Value>) -> String {
    match id {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => "null".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use onesync_fs_local::fakes::InMemoryLocalFs;
    use onesync_protocol::rpc::{JsonRpcRequest, JsonRpcResponse, METHOD_NOT_FOUND};
    use onesync_state::fakes::InMemoryStore;
    use onesync_time::{SystemClock, UlidGenerator};

    use super::*;

    /// Captures audit events to a Vec for assertion.
    #[derive(Default)]
    struct NullAuditSink;
    impl onesync_core::ports::AuditSink for NullAuditSink {
        fn emit(&self, _event: onesync_protocol::audit::AuditEvent) {}
    }

    fn ctx() -> DispatchCtx {
        DispatchCtx {
            started_at: Instant::now(),
            state: Arc::new(InMemoryStore::new()),
            local_fs: Arc::new(InMemoryLocalFs::new()),
            clock: Arc::new(SystemClock),
            ids: Arc::new(UlidGenerator::default()),
            audit: Arc::new(NullAuditSink),
        }
    }

    #[test]
    fn health_ping_returns_ok() {
        let req = JsonRpcRequest::new("1", "health.ping", serde_json::Value::Null);
        let resp = dispatch(&req, &ctx());
        assert!(matches!(resp, JsonRpcResponse::Ok(_)));
    }

    #[test]
    fn unknown_method_returns_method_not_found() {
        let req = JsonRpcRequest::new("2", "unknown.method", serde_json::Value::Null);
        let resp = dispatch(&req, &ctx());
        match resp {
            JsonRpcResponse::Err(e) => assert_eq!(e.error.code, METHOD_NOT_FOUND),
            JsonRpcResponse::Ok(_) => unreachable!("expected error response"),
        }
    }
}
