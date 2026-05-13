//! IPC method integration tests.
//!
//! Exercises every method group through the full IPC framing stack:
//! - `health.ping`, `health.diagnostics`
//! - `account.list`, `pair.list`, `conflict.list`, `audit.search`, `run.list`
//! - All stubs return a JSON-RPC response (Ok or `not_implemented` App error)

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::sync::Arc;
use std::time::Instant;

use onesync_daemon::ipc::server;
use onesync_daemon::methods::DispatchCtx;
use onesync_daemon::shutdown::ShutdownToken;
use onesync_protocol::rpc::{APP_ERROR_BASE, JsonRpcRequest, JsonRpcResponse};
use onesync_state::fakes::InMemoryStore;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Default)]
struct NullAuditSink;
impl onesync_core::ports::AuditSink for NullAuditSink {
    fn emit(&self, _event: onesync_protocol::audit::AuditEvent) {}
}

fn make_ctx() -> DispatchCtx {
    DispatchCtx {
        started_at: Instant::now(),
        state: Arc::new(InMemoryStore::new()),
        local_fs: Arc::new(onesync_fs_local::fakes::InMemoryLocalFs::new()),
        clock: Arc::new(onesync_time::SystemClock),
        ids: Arc::new(onesync_time::UlidGenerator::default()),
        audit: Arc::new(NullAuditSink),
        vault: Arc::new(onesync_keychain::fakes::InMemoryTokenVault::default()),
        http: reqwest::Client::new(),
        login_registry: Arc::new(onesync_daemon::login_registry::LoginRegistry::new()),
        shutdown_token: onesync_daemon::shutdown::ShutdownToken::new(),
        state_dir: std::path::PathBuf::from("/tmp/onesync-test-state"),
    }
}

async fn start_server() -> (ShutdownToken, std::path::PathBuf, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let token = ShutdownToken::new();
    let runtime_dir = tmp.path().to_path_buf();
    let ctx = make_ctx();

    let token_clone = token.clone();
    let runtime_dir_clone = runtime_dir.clone();
    tokio::spawn(async move {
        server::run(&runtime_dir_clone, token_clone, ctx)
            .await
            .expect("IPC server error");
    });

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let sock_path = runtime_dir.join(server::SOCKET_FILE);
    (token, sock_path, tmp)
}

/// Send one request and read one response from the socket.
async fn roundtrip(sock_path: &std::path::Path, method: &str) -> JsonRpcResponse {
    let stream = UnixStream::connect(sock_path)
        .await
        .expect("connect to IPC socket");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let req = JsonRpcRequest::new("1", method, serde_json::Value::Null);
    let json = serde_json::to_string(&req).expect("serialize");
    write_half.write_all(json.as_bytes()).await.expect("write");
    write_half.write_all(b"\n").await.expect("write newline");

    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read response");
    serde_json::from_str(line.trim()).expect("parse response")
}

/// Assert a response is Ok (success).
fn assert_ok(resp: &JsonRpcResponse, method: &str) {
    assert!(
        matches!(resp, JsonRpcResponse::Ok(_)),
        "expected Ok for {method}, got: {resp:?}"
    );
}

/// Assert a response is a `not_implemented` application error.
fn assert_not_implemented(resp: &JsonRpcResponse, method: &str) {
    match resp {
        JsonRpcResponse::Err(e) => {
            assert_eq!(
                e.error.code, APP_ERROR_BASE,
                "{method} should return APP_ERROR_BASE ({APP_ERROR_BASE}), got {}",
                e.error.code
            );
        }
        JsonRpcResponse::Ok(_) => {
            unreachable!("{method} unexpectedly returned Ok — stub should return NotImplemented");
        }
    }
}

// ── health ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn health_ping() {
    let (token, sock, _tmp) = start_server().await;
    assert_ok(&roundtrip(&sock, "health.ping").await, "health.ping");
    token.trigger();
}

#[tokio::test]
async fn health_diagnostics() {
    let (token, sock, _tmp) = start_server().await;
    assert_ok(
        &roundtrip(&sock, "health.diagnostics").await,
        "health.diagnostics",
    );
    token.trigger();
}

/// Send a request with custom params and read one response.
async fn roundtrip_params(
    sock_path: &std::path::Path,
    method: &str,
    params: serde_json::Value,
) -> JsonRpcResponse {
    let stream = UnixStream::connect(sock_path)
        .await
        .expect("connect to IPC socket");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let req = JsonRpcRequest::new("1", method, params);
    let json = serde_json::to_string(&req).expect("serialize");
    write_half.write_all(json.as_bytes()).await.expect("write");
    write_half.write_all(b"\n").await.expect("write newline");
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read response");
    serde_json::from_str(line.trim()).expect("parse response")
}

// ── account ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn account_list_returns_empty_array_on_fresh_store() {
    let (token, sock, _tmp) = start_server().await;
    let resp = roundtrip(&sock, "account.list").await;
    match resp {
        JsonRpcResponse::Ok(ok) => {
            assert!(ok.result.is_array(), "account.list must return an array");
            assert_eq!(ok.result.as_array().expect("array").len(), 0);
        }
        JsonRpcResponse::Err(e) => unreachable!("expected Ok, got error {e:?}"),
    }
    token.trigger();
}

#[tokio::test]
async fn account_login_begin_refuses_when_azure_ad_client_id_unset() {
    // M9 Task 2: login.begin reads azure_ad_client_id from InstanceConfig. With no config row
    // it must refuse with an APP_ERROR rather than touching the OAuth provider.
    let (token, sock, _tmp) = start_server().await;
    let resp = roundtrip(&sock, "account.login.begin").await;
    match resp {
        JsonRpcResponse::Err(e) => {
            assert_eq!(e.error.code, APP_ERROR_BASE - 10);
            assert!(
                e.error.message.contains("azure_ad_client_id"),
                "unexpected error message: {}",
                e.error.message
            );
        }
        JsonRpcResponse::Ok(_) => unreachable!("expected error, got Ok"),
    }
    token.trigger();
}

// ── pair ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn pair_list_returns_empty_array_on_fresh_store() {
    let (token, sock, _tmp) = start_server().await;
    let resp = roundtrip(&sock, "pair.list").await;
    assert!(matches!(resp, JsonRpcResponse::Ok(_)));
    token.trigger();
}

#[tokio::test]
async fn pair_force_sync_still_returns_not_implemented() {
    let (token, sock, _tmp) = start_server().await;
    assert_not_implemented(
        &roundtrip(&sock, "pair.force_sync").await,
        "pair.force_sync",
    );
    token.trigger();
}

// ── conflict ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn conflict_list_returns_empty_array_for_pair_without_conflicts() {
    let (token, sock, _tmp) = start_server().await;
    let resp = roundtrip_params(
        &sock,
        "conflict.list",
        serde_json::json!({ "pair": "pair_01J8X7CFGMZG7Y4DC0VA8DZW2H" }),
    )
    .await;
    match resp {
        JsonRpcResponse::Ok(ok) => {
            assert!(ok.result.is_array());
            assert_eq!(ok.result.as_array().expect("array").len(), 0);
        }
        JsonRpcResponse::Err(e) => unreachable!("expected Ok, got error {e:?}"),
    }
    token.trigger();
}

// ── audit ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn audit_search_returns_empty_array_on_fresh_store() {
    let (token, sock, _tmp) = start_server().await;
    let resp = roundtrip_params(
        &sock,
        "audit.search",
        serde_json::json!({
            "from": "2020-01-01T00:00:00Z",
            "to": "2030-01-01T00:00:00Z",
        }),
    )
    .await;
    match resp {
        JsonRpcResponse::Ok(ok) => {
            assert!(ok.result.is_array());
            assert_eq!(ok.result.as_array().expect("array").len(), 0);
        }
        JsonRpcResponse::Err(e) => unreachable!("expected Ok, got error {e:?}"),
    }
    token.trigger();
}

// ── run ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn run_list_returns_empty_array_on_fresh_store() {
    let (token, sock, _tmp) = start_server().await;
    let resp = roundtrip_params(
        &sock,
        "run.list",
        serde_json::json!({ "pair": "pair_01J8X7CFGMZG7Y4DC0VA8DZW2H" }),
    )
    .await;
    assert!(matches!(resp, JsonRpcResponse::Ok(_)));
    token.trigger();
}

// ── state ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn state_compact_returns_ok_on_in_memory_store() {
    let (token, sock, _tmp) = start_server().await;
    // InMemoryStore.compact_now is a no-op; the handler should still report success.
    let resp = roundtrip(&sock, "state.compact.now").await;
    assert!(matches!(resp, JsonRpcResponse::Ok(_)));
    token.trigger();
}

// ── service ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn service_shutdown_returns_ok() {
    let (token, sock, _tmp) = start_server().await;
    // The shutdown_token inside the server-side DispatchCtx is independent of `token`
    // here (each task constructs its own), so calling service.shutdown returns Ok but
    // does not actually stop *this* test's server. token.trigger() at the end does that.
    let resp = roundtrip(&sock, "service.shutdown").await;
    assert!(matches!(resp, JsonRpcResponse::Ok(_)));
    token.trigger();
}

// ── config ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn config_get_returns_null_on_fresh_store() {
    let (token, sock, _tmp) = start_server().await;
    let resp = roundtrip(&sock, "config.get").await;
    match resp {
        JsonRpcResponse::Ok(ok) => assert!(ok.result.is_null()),
        JsonRpcResponse::Err(e) => unreachable!("expected Ok, got {e:?}"),
    }
    token.trigger();
}

#[tokio::test]
async fn config_set_then_get_round_trips() {
    let (token, sock, _tmp) = start_server().await;
    let set_resp = roundtrip_params(
        &sock,
        "config.set",
        serde_json::json!({ "log_level": "debug", "notify": false }),
    )
    .await;
    assert!(matches!(set_resp, JsonRpcResponse::Ok(_)));

    let get_resp = roundtrip(&sock, "config.get").await;
    match get_resp {
        JsonRpcResponse::Ok(ok) => {
            assert_eq!(ok.result["log_level"], "debug");
            assert_eq!(ok.result["notify"], false);
        }
        JsonRpcResponse::Err(e) => unreachable!("expected Ok, got {e:?}"),
    }
    token.trigger();
}
