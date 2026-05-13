//! Per-connection subscription writer integration test (M12b Task D).
//!
//! Verifies the end-to-end path: `audit.tail` registers a subscription, the
//! `SubscriptionRegistry::broadcast` pushes a notification, the per-connection forwarder
//! pumps it into the connection's notification mpsc, and the writer task serialises an
//! `audit.event` JSON-RPC notification onto the Unix socket.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::sync::Arc;
use std::time::Instant;

use onesync_daemon::ipc::server;
use onesync_daemon::ipc::subscriptions::SubscriptionRegistry;
use onesync_daemon::methods::DispatchCtx;
use onesync_daemon::shutdown::ShutdownToken;
use onesync_protocol::rpc::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use onesync_state::fakes::InMemoryStore;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Default)]
struct NullAuditSink;
impl onesync_core::ports::AuditSink for NullAuditSink {
    fn emit(&self, _event: onesync_protocol::audit::AuditEvent) {}
}

fn make_ctx(subscriptions: SubscriptionRegistry) -> DispatchCtx {
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
        scheduler: onesync_daemon::scheduler::SchedulerHandle::for_tests(),
        subscriptions,
    }
}

#[tokio::test]
async fn audit_tail_streams_notifications_through_connection_writer() {
    let tmp = TempDir::new().expect("tempdir");
    let token = ShutdownToken::new();
    let runtime_dir = tmp.path().to_path_buf();
    let subscriptions = SubscriptionRegistry::new();
    let ctx = make_ctx(subscriptions.clone());

    let token_clone = token.clone();
    let runtime_dir_clone = runtime_dir.clone();
    tokio::spawn(async move {
        server::run(&runtime_dir_clone, token_clone, ctx)
            .await
            .expect("IPC server error");
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let sock_path = runtime_dir.join(server::SOCKET_FILE);
    let stream = UnixStream::connect(&sock_path)
        .await
        .expect("connect to IPC socket");
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // 1. audit.tail registers a subscription. Response is a JSON-RPC Ok with subscription_id.
    let req = JsonRpcRequest::new("1", "audit.tail", serde_json::Value::Null);
    write_half
        .write_all(serde_json::to_string(&req).unwrap().as_bytes())
        .await
        .unwrap();
    write_half.write_all(b"\n").await.unwrap();

    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let resp: JsonRpcResponse = serde_json::from_str(line.trim()).expect("parse response");
    match resp {
        JsonRpcResponse::Ok(ok) => {
            assert!(ok.result.get("subscription_id").is_some());
        }
        JsonRpcResponse::Err(e) => unreachable!("audit.tail failed: {e:?}"),
    }

    // Give the forwarder task a moment to attach to the subscription.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // 2. Broadcast an audit.event notification.
    let notif = JsonRpcNotification::new(
        "audit.event",
        serde_json::json!({ "kind": "test.broadcast", "level": "info" }),
    );
    subscriptions.broadcast(&notif);

    // 3. The next frame from the connection must be the notification.
    let mut line = String::new();
    let read_fut = reader.read_line(&mut line);
    let line_len = tokio::time::timeout(std::time::Duration::from_secs(2), read_fut)
        .await
        .expect("notification arrives within 2 s")
        .expect("read_line ok");
    assert!(line_len > 0, "writer produced an empty frame");

    let parsed: serde_json::Value = serde_json::from_str(line.trim()).expect("notification json");
    assert_eq!(parsed["method"], "audit.event");
    assert_eq!(parsed["params"]["kind"], "test.broadcast");

    token.trigger();
}
