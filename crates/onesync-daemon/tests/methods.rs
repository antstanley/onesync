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

fn make_ctx() -> DispatchCtx {
    DispatchCtx {
        started_at: Instant::now(),
        state: Arc::new(InMemoryStore::new()),
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

// ── account ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn account_list_returns_not_implemented() {
    let (token, sock, _tmp) = start_server().await;
    assert_not_implemented(&roundtrip(&sock, "account.list").await, "account.list");
    token.trigger();
}

// ── pair ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn pair_list_returns_not_implemented() {
    let (token, sock, _tmp) = start_server().await;
    assert_not_implemented(&roundtrip(&sock, "pair.list").await, "pair.list");
    token.trigger();
}

// ── conflict ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn conflict_list_returns_not_implemented() {
    let (token, sock, _tmp) = start_server().await;
    assert_not_implemented(&roundtrip(&sock, "conflict.list").await, "conflict.list");
    token.trigger();
}

// ── audit ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn audit_search_returns_not_implemented() {
    let (token, sock, _tmp) = start_server().await;
    assert_not_implemented(&roundtrip(&sock, "audit.search").await, "audit.search");
    token.trigger();
}

// ── run ───────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn run_list_returns_not_implemented() {
    let (token, sock, _tmp) = start_server().await;
    assert_not_implemented(&roundtrip(&sock, "run.list").await, "run.list");
    token.trigger();
}

// ── state ─────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn state_backup_returns_not_implemented() {
    let (token, sock, _tmp) = start_server().await;
    assert_not_implemented(&roundtrip(&sock, "state.backup").await, "state.backup");
    token.trigger();
}

// ── service ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn service_shutdown_returns_not_implemented() {
    let (token, sock, _tmp) = start_server().await;
    assert_not_implemented(
        &roundtrip(&sock, "service.shutdown").await,
        "service.shutdown",
    );
    token.trigger();
}
