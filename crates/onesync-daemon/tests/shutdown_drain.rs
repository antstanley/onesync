//! Graceful shutdown integration tests.
//!
//! Verifies that:
//! - Triggering a `ShutdownToken` causes the IPC server to stop accepting.
//! - The socket file is removed after shutdown.
//! - The server exits within a reasonable timeout.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use onesync_core::limits::SHUTDOWN_DRAIN_TIMEOUT_S;
use onesync_daemon::ipc::server;
use onesync_daemon::methods::DispatchCtx;
use onesync_daemon::shutdown::ShutdownToken;
use onesync_state::fakes::InMemoryStore;
use tempfile::TempDir;

fn make_ctx() -> DispatchCtx {
    DispatchCtx {
        started_at: Instant::now(),
        state: Arc::new(InMemoryStore::new()),
    }
}

#[tokio::test]
async fn server_exits_within_shutdown_drain_timeout() {
    let tmp = TempDir::new().expect("tempdir");
    let token = ShutdownToken::new();
    let runtime_dir = tmp.path().to_path_buf();
    let ctx = make_ctx();

    let token_clone = token.clone();
    let runtime_dir_clone = runtime_dir.clone();
    let handle = tokio::spawn(async move {
        server::run(&runtime_dir_clone, token_clone, ctx)
            .await
            .expect("server error");
    });

    // Wait for the socket to appear.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(runtime_dir.join(server::SOCKET_FILE).exists());

    // Trigger shutdown.
    let start = Instant::now();
    token.trigger();

    // Wait for the server task to finish.
    tokio::time::timeout(Duration::from_secs(SHUTDOWN_DRAIN_TIMEOUT_S), handle)
        .await
        .expect("server should exit within SHUTDOWN_DRAIN_TIMEOUT_S")
        .expect("server task panicked");

    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(SHUTDOWN_DRAIN_TIMEOUT_S),
        "shutdown took {elapsed:?}, expected < {SHUTDOWN_DRAIN_TIMEOUT_S}s"
    );
}

#[tokio::test]
async fn socket_file_removed_after_shutdown() {
    let tmp = TempDir::new().expect("tempdir");
    let token = ShutdownToken::new();
    let runtime_dir = tmp.path().to_path_buf();
    let ctx = make_ctx();

    let token_clone = token.clone();
    let runtime_dir_clone = runtime_dir.clone();
    let handle = tokio::spawn(async move {
        server::run(&runtime_dir_clone, token_clone, ctx)
            .await
            .expect("server error");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    let sock_path = runtime_dir.join(server::SOCKET_FILE);
    assert!(sock_path.exists(), "socket should exist before shutdown");

    token.trigger();
    handle.await.expect("server task panicked");

    assert!(
        !sock_path.exists(),
        "socket file should be removed after shutdown"
    );
}

#[tokio::test]
async fn shutdown_token_is_triggered_at_most_once() {
    let token = ShutdownToken::new();
    token.trigger();
    token.trigger(); // second call should not panic
    token.trigger(); // third call should not panic
}

#[tokio::test]
async fn new_connections_rejected_after_shutdown() {
    let tmp = TempDir::new().expect("tempdir");
    let token = ShutdownToken::new();
    let runtime_dir = tmp.path().to_path_buf();
    let ctx = make_ctx();

    let token_clone = token.clone();
    let runtime_dir_clone = runtime_dir.clone();
    let handle = tokio::spawn(async move {
        server::run(&runtime_dir_clone, token_clone, ctx)
            .await
            .expect("server error");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Trigger shutdown and wait for server to stop.
    token.trigger();
    handle.await.expect("server task panicked");

    // After shutdown, the socket file is gone — new connections fail.
    let sock_path = runtime_dir.join(server::SOCKET_FILE);
    let connect_result = tokio::net::UnixStream::connect(&sock_path).await;
    assert!(
        connect_result.is_err(),
        "should not be able to connect to shut-down socket"
    );
}
