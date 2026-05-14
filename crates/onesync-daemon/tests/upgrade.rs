//! Integration test for service.upgrade.prepare + service.upgrade.commit (M12b Task H).
//!
//! Exercises the two-phase flow end-to-end without actually exec'ing — the test stops
//! after asserting that commit succeeded and the shutdown token fired.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::os::unix::fs::PermissionsExt as _;
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

#[derive(Default)]
struct NullAuditSink;
impl onesync_core::ports::AuditSink for NullAuditSink {
    fn emit(&self, _event: onesync_protocol::audit::AuditEvent) {}
}

fn make_ctx(token: ShutdownToken) -> DispatchCtx {
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
        shutdown_token: token,
        state_dir: std::path::PathBuf::from("/tmp/onesync-test-state"),
        scheduler: onesync_daemon::scheduler::SchedulerHandle::for_tests(),
        subscriptions: onesync_daemon::ipc::subscriptions::SubscriptionRegistry::new(),
        upgrade_staging: std::sync::Arc::new(std::sync::Mutex::new(None)),
    }
}

async fn call(
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
    write_half.write_all(b"\n").await.expect("newline");
    let mut line = String::new();
    reader.read_line(&mut line).await.expect("read");
    serde_json::from_str(line.trim()).expect("parse response")
}

#[tokio::test]
async fn upgrade_prepare_then_commit_triggers_shutdown() {
    // Stage an executable file (the test never actually exec's it — commit only
    // signals shutdown; the exec happens in main after the IPC server joins).
    let stage_dir = TempDir::new().expect("tempdir");
    let staged = stage_dir.path().join("onesyncd.next");
    std::fs::write(&staged, b"#!/bin/sh\nexit 0\n").expect("write staged binary");
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    // Boot the IPC server.
    let runtime_tmp = TempDir::new().expect("runtime tempdir");
    let token = ShutdownToken::new();
    let runtime_dir = runtime_tmp.path().to_path_buf();
    let ctx = make_ctx(token.clone());

    let token_clone = token.clone();
    let runtime_dir_clone = runtime_dir.clone();
    let server_handle = tokio::spawn(async move {
        server::run(&runtime_dir_clone, token_clone, ctx)
            .await
            .expect("IPC server error");
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let sock_path = runtime_dir.join(server::SOCKET_FILE);

    // prepare → ok
    let resp = call(
        &sock_path,
        "service.upgrade.prepare",
        serde_json::json!({ "binary_path": staged.display().to_string() }),
    )
    .await;
    match resp {
        JsonRpcResponse::Ok(o) => {
            assert_eq!(o.result["ok"], true);
            assert_eq!(
                o.result["staged_path"].as_str(),
                Some(staged.display().to_string().as_str())
            );
        }
        JsonRpcResponse::Err(e) => unreachable!("prepare failed: {e:?}"),
    }

    // A subsequent prepare with a bogus path fails — the previous staged path stays.
    let resp = call(
        &sock_path,
        "service.upgrade.prepare",
        serde_json::json!({ "binary_path": "/no/such/binary" }),
    )
    .await;
    assert!(matches!(resp, JsonRpcResponse::Err(_)));

    // commit → ok; shutdown token must fire as a side-effect.
    let mut shutdown_rx = token.subscribe();
    let resp = call(&sock_path, "service.upgrade.commit", serde_json::json!({})).await;
    match resp {
        JsonRpcResponse::Ok(o) => {
            assert_eq!(o.result["ok"], true);
        }
        JsonRpcResponse::Err(e) => unreachable!("commit failed: {e:?}"),
    }
    let triggered =
        tokio::time::timeout(std::time::Duration::from_secs(1), shutdown_rx.recv()).await;
    assert!(
        triggered.is_ok(),
        "shutdown token did not fire after commit"
    );

    let _ = server_handle.await;
}

#[tokio::test]
async fn upgrade_commit_without_prepare_returns_error() {
    let runtime_tmp = TempDir::new().expect("runtime tempdir");
    let token = ShutdownToken::new();
    let runtime_dir = runtime_tmp.path().to_path_buf();
    let ctx = make_ctx(token.clone());

    let token_clone = token.clone();
    let runtime_dir_clone = runtime_dir.clone();
    let server_handle = tokio::spawn(async move {
        server::run(&runtime_dir_clone, token_clone, ctx)
            .await
            .expect("IPC server error");
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let sock_path = runtime_dir.join(server::SOCKET_FILE);

    let resp = call(&sock_path, "service.upgrade.commit", serde_json::json!({})).await;
    match resp {
        JsonRpcResponse::Err(_) => {}
        JsonRpcResponse::Ok(o) => unreachable!("expected error, got Ok: {:?}", o.result),
    }
    token.trigger();
    let _ = server_handle.await;
}
