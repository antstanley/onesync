//! IPC handshake integration test.
//!
//! Spins up the IPC server in-process with fake adapters, connects via a
//! `UnixStream`, sends `health.ping`, and asserts a well-formed JSON-RPC
//! success response.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::sync::Arc;
use std::time::Instant;

use onesync_daemon::ipc::server;
use onesync_daemon::methods::DispatchCtx;
use onesync_daemon::shutdown::ShutdownToken;
use onesync_protocol::rpc::{JsonRpcRequest, JsonRpcResponse};
use onesync_state::fakes::InMemoryStore;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Build a `DispatchCtx` backed by an in-memory state store.
fn make_ctx() -> DispatchCtx {
    DispatchCtx {
        started_at: Instant::now(),
        state: Arc::new(InMemoryStore::new()),
    }
}

/// Start the IPC server task and return (token, `socket_path`, tempdir).
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

    // Wait for the server to bind the socket.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let sock_path = runtime_dir.join(server::SOCKET_FILE);
    (token, sock_path, tmp)
}

#[tokio::test]
async fn health_ping_returns_ok_response() {
    let (token, sock_path, _tmp) = start_server().await;

    let stream = UnixStream::connect(&sock_path)
        .await
        .expect("connect to IPC socket");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Send health.ping request.
    let req = JsonRpcRequest::new("ping-1", "health.ping", serde_json::Value::Null);
    let json = serde_json::to_string(&req).expect("serialize");
    write_half
        .write_all(json.as_bytes())
        .await
        .expect("write request");
    write_half.write_all(b"\n").await.expect("write newline");

    // Read response.
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read response");

    let resp: JsonRpcResponse = serde_json::from_str(line.trim()).expect("parse response");
    assert!(
        matches!(resp, JsonRpcResponse::Ok(_)),
        "expected Ok response to health.ping, got: {resp:?}"
    );

    token.trigger();
}

#[tokio::test]
async fn unknown_method_returns_error_response() {
    let (token, sock_path, _tmp) = start_server().await;

    let stream = UnixStream::connect(&sock_path)
        .await
        .expect("connect to IPC socket");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    let req = JsonRpcRequest::new("err-1", "no.such.method", serde_json::Value::Null);
    let json = serde_json::to_string(&req).expect("serialize");
    write_half
        .write_all(json.as_bytes())
        .await
        .expect("write request");
    write_half.write_all(b"\n").await.expect("write newline");

    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read response");

    let resp: JsonRpcResponse = serde_json::from_str(line.trim()).expect("parse response");
    assert!(
        matches!(resp, JsonRpcResponse::Err(_)),
        "expected Err response to unknown method, got: {resp:?}"
    );

    token.trigger();
}

#[tokio::test]
async fn malformed_json_returns_parse_error() {
    let (token, sock_path, _tmp) = start_server().await;

    let stream = UnixStream::connect(&sock_path)
        .await
        .expect("connect to IPC socket");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    write_half
        .write_all(b"not valid json\n")
        .await
        .expect("write");

    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read response");

    let resp: JsonRpcResponse = serde_json::from_str(line.trim()).expect("parse response");
    match resp {
        JsonRpcResponse::Err(e) => {
            assert_eq!(
                e.error.code,
                onesync_protocol::rpc::PARSE_ERROR,
                "expected PARSE_ERROR (-32700)"
            );
        }
        JsonRpcResponse::Ok(_) => unreachable!("expected error"),
    }

    token.trigger();
}
