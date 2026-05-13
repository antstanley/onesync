//! Integration test for pair.subscribe and conflict.subscribe (M10b carry-overs).
//!
//! Verifies that the two new handlers register subscriptions, the forwarder tasks
//! correctly filter the global broadcast stream, and matching audit events reach the
//! client as `audit.event` notification frames.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::sync::Arc;
use std::time::Instant;

use chrono::{TimeZone, Utc};
use onesync_core::ports::IdGenerator as _;
use onesync_daemon::ipc::server;
use onesync_daemon::ipc::subscriptions::SubscriptionRegistry;
use onesync_daemon::methods::DispatchCtx;
use onesync_daemon::shutdown::ShutdownToken;
use onesync_protocol::audit::AuditEvent;
use onesync_protocol::enums::AuditLevel;
use onesync_protocol::id::{AuditEventId, AuditTag, PairId, PairTag};
use onesync_protocol::primitives::Timestamp;
use onesync_protocol::rpc::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use onesync_state::fakes::InMemoryStore;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Default)]
struct NullAuditSink;
impl onesync_core::ports::AuditSink for NullAuditSink {
    fn emit(&self, _event: AuditEvent) {}
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
        upgrade_staging: std::sync::Arc::new(std::sync::Mutex::new(None)),
    }
}

async fn start(subscriptions: SubscriptionRegistry) -> (ShutdownToken, std::path::PathBuf, TempDir) {
    let tmp = TempDir::new().expect("tempdir");
    let token = ShutdownToken::new();
    let ctx = make_ctx(subscriptions);
    let token_clone = token.clone();
    let runtime_dir = tmp.path().to_path_buf();
    let runtime_dir_clone = runtime_dir.clone();
    tokio::spawn(async move {
        server::run(&runtime_dir_clone, token_clone, ctx).await.unwrap();
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let sock_path = runtime_dir.join(server::SOCKET_FILE);
    (token, sock_path, tmp)
}

async fn open(sock: &std::path::Path) -> (BufReader<tokio::net::unix::OwnedReadHalf>, tokio::net::unix::OwnedWriteHalf) {
    let stream = UnixStream::connect(sock).await.unwrap();
    let (read_half, write_half) = stream.into_split();
    (BufReader::new(read_half), write_half)
}

async fn send(
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    method: &str,
    params: serde_json::Value,
) {
    let req = JsonRpcRequest::new("1", method, params);
    let s = serde_json::to_string(&req).unwrap();
    write_half.write_all(s.as_bytes()).await.unwrap();
    write_half.write_all(b"\n").await.unwrap();
}

async fn read_response(reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>) -> JsonRpcResponse {
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(line.trim()).expect("response json")
}

async fn read_next_frame(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
    timeout_ms: u64,
) -> Option<serde_json::Value> {
    let mut line = String::new();
    let read_fut = reader.read_line(&mut line);
    let n = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), read_fut)
        .await
        .ok()?
        .ok()?;
    if n == 0 {
        return None;
    }
    serde_json::from_str(line.trim()).ok()
}

fn pair_id() -> PairId {
    onesync_time::UlidGenerator::default().new_id::<PairTag>()
}

fn audit_event(kind: &str, pair: Option<PairId>) -> AuditEvent {
    let id: AuditEventId = onesync_time::UlidGenerator::default().new_id::<AuditTag>();
    let ts = Timestamp::from_datetime(Utc.timestamp_opt(0, 0).unwrap());
    AuditEvent {
        id,
        ts,
        level: AuditLevel::Info,
        kind: kind.to_owned(),
        pair_id: pair,
        payload: serde_json::Map::new(),
    }
}

#[tokio::test]
async fn pair_subscribe_filters_by_pair_id() {
    let reg = SubscriptionRegistry::new();
    let (token, sock, _tmp) = start(reg.clone()).await;
    let (mut reader, mut write_half) = open(&sock).await;

    let target = pair_id();
    let other = pair_id();

    send(
        &mut write_half,
        "pair.subscribe",
        serde_json::json!({ "pair": target.to_string() }),
    )
    .await;
    let resp = read_response(&mut reader).await;
    assert!(matches!(resp, JsonRpcResponse::Ok(_)));

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // Broadcast: one for the target pair, one for a different pair, one without pair_id.
    let to_value = |e: &AuditEvent| serde_json::to_value(e).unwrap();
    reg.broadcast(&JsonRpcNotification::new(
        "audit.event",
        to_value(&audit_event("cycle.started", Some(target))),
    ));
    reg.broadcast(&JsonRpcNotification::new(
        "audit.event",
        to_value(&audit_event("cycle.started", Some(other))),
    ));
    reg.broadcast(&JsonRpcNotification::new(
        "audit.event",
        to_value(&audit_event("system.event", None)),
    ));

    // Only the target-pair frame should arrive.
    let frame = read_next_frame(&mut reader, 1500)
        .await
        .expect("frame arrives");
    assert_eq!(frame["method"], "audit.event");
    assert_eq!(
        frame["params"]["pair_id"].as_str(),
        Some(target.to_string().as_str())
    );

    // No second frame within 200 ms — the other-pair and pair-less events were filtered.
    let extra = read_next_frame(&mut reader, 200).await;
    assert!(extra.is_none(), "expected no further frames, got {extra:?}");

    token.trigger();
}

#[tokio::test]
async fn conflict_subscribe_filters_to_conflict_kinds() {
    let reg = SubscriptionRegistry::new();
    let (token, sock, _tmp) = start(reg.clone()).await;
    let (mut reader, mut write_half) = open(&sock).await;

    send(&mut write_half, "conflict.subscribe", serde_json::Value::Null).await;
    let resp = read_response(&mut reader).await;
    assert!(matches!(resp, JsonRpcResponse::Ok(_)));

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // A cycle event must NOT pass; a case-collision event must.
    reg.broadcast(&JsonRpcNotification::new(
        "audit.event",
        serde_json::to_value(audit_event("cycle.finished", None)).unwrap(),
    ));
    reg.broadcast(&JsonRpcNotification::new(
        "audit.event",
        serde_json::to_value(audit_event("local.case_collision.renamed", Some(pair_id())))
            .unwrap(),
    ));

    let frame = read_next_frame(&mut reader, 1500)
        .await
        .expect("frame arrives");
    assert_eq!(
        frame["params"]["kind"].as_str(),
        Some("local.case_collision.renamed")
    );

    let extra = read_next_frame(&mut reader, 200).await;
    assert!(extra.is_none(), "expected no further frames, got {extra:?}");

    token.trigger();
}
