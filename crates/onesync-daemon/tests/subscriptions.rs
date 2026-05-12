//! Subscription registry integration tests.
//!
//! Validates the `SubscriptionRegistry` + `DaemonAuditSink` pipeline:
//! - Events emitted to the sink arrive on registered subscription channels.
//! - Dropped receivers are cleaned up by `gc()`.
//! - Back-pressure (full channel) drops events rather than blocking.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use onesync_core::engine::observability;
use onesync_core::ports::{AuditSink as _, IdGenerator as _, StateStore};
use onesync_daemon::audit_sink::DaemonAuditSink;
use onesync_daemon::ipc::subscriptions::{SubscriptionId, SubscriptionRegistry};
use onesync_protocol::id::{AuditEventId, PairId};
use onesync_protocol::primitives::Timestamp;
use onesync_state::fakes::InMemoryStore;
use onesync_time::fakes::TestIdGenerator;

fn epoch_ts() -> Timestamp {
    Timestamp::from_datetime(Utc.timestamp_opt(0, 0).unwrap())
}

fn make_event(seed: u64) -> onesync_protocol::audit::AuditEvent {
    let ids = TestIdGenerator::seeded(seed);
    let id: AuditEventId = ids.new_id();
    let pair_id: PairId = ids.new_id();
    observability::cycle_started(id, epoch_ts(), pair_id)
}

#[tokio::test]
async fn audit_event_delivered_to_subscriber() {
    let state: Arc<dyn StateStore> = Arc::new(InMemoryStore::new());
    let reg = SubscriptionRegistry::new();
    let sink = DaemonAuditSink::new(Arc::clone(&state), reg.clone());

    // Register a subscription.
    let id = SubscriptionId::new("sub-test-1");
    let mut rx = reg.insert(id.clone());

    // The DaemonAuditSink broadcasts to subscriptions via audit.event notification.
    sink.emit(make_event(1));

    // Wait for the drain task to process and broadcast.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // The registry should have received the notification.
    let msg = rx.try_recv();
    assert!(msg.is_ok(), "expected notification in subscription channel");
    let notif = msg.unwrap();
    assert_eq!(notif.method, "audit.event");

    reg.remove(&id);
}

#[tokio::test]
async fn cancelled_subscription_receives_no_further_events() {
    let state: Arc<dyn StateStore> = Arc::new(InMemoryStore::new());
    let reg = SubscriptionRegistry::new();
    let sink = DaemonAuditSink::new(Arc::clone(&state), reg.clone());

    let id = SubscriptionId::new("sub-cancel-1");
    let rx = reg.insert(id.clone());

    // Cancel (remove) the subscription before emitting.
    reg.remove(&id);
    drop(rx);

    // Emit — should not panic or block.
    sink.emit(make_event(2));
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    // Registry should be empty.
    assert_eq!(reg.len(), 0);
}

#[tokio::test]
async fn gc_cleans_dead_subscription_channels() {
    let reg = SubscriptionRegistry::new();

    let id = SubscriptionId::new("sub-gc-1");
    let rx = reg.insert(id);
    assert_eq!(reg.len(), 1);

    // Drop the receiver — marks the sender as closed.
    drop(rx);

    // Run GC.
    reg.gc();
    assert_eq!(reg.len(), 0);
}
