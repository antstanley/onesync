//! `DaemonAuditSink` — bridges the engine's synchronous `AuditSink` port to
//! async persistence and live subscription fanout.
//!
//! ## Design
//!
//! `emit` is a synchronous method (required by the port trait), so it sends
//! events through a [`tokio::sync::mpsc::UnboundedSender`] to a background
//! Tokio task that calls [`StateStore::audit_append`] and fans events out to
//! any live [`audit.tail`](crate::ipc::subscriptions) subscribers.

// LINT: DaemonAuditSink is wired into DaemonPorts in Task 14; DRAIN_CAPACITY
//       is a public constant for callers.
#![allow(dead_code)]

use std::sync::Arc;

use onesync_core::ports::{AuditSink, StateStore};
use onesync_protocol::audit::AuditEvent;
use onesync_protocol::rpc::JsonRpcNotification;
use tokio::sync::mpsc;

use crate::ipc::subscriptions::SubscriptionRegistry;

/// Synchronous `AuditSink` that forwards events to an async drain task.
#[derive(Clone)]
pub struct DaemonAuditSink {
    tx: mpsc::UnboundedSender<AuditEvent>,
}

impl DaemonAuditSink {
    /// Construct a new sink and spawn the background drain task.
    ///
    /// The drain task runs until the sender is dropped.
    #[must_use]
    pub fn new(state: Arc<dyn StateStore>, subscriptions: SubscriptionRegistry) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(drain_task(rx, state, subscriptions));
        Self { tx }
    }
}

impl AuditSink for DaemonAuditSink {
    fn emit(&self, event: AuditEvent) {
        // `send` on an unbounded channel only fails if the receiver is gone
        // (i.e., the drain task has exited).  Log and discard in that case.
        if self.tx.send(event).is_err() {
            tracing::warn!("audit drain task has exited; event discarded");
        }
    }
}

/// Background task: persist each event and fan out to subscribers.
async fn drain_task(
    mut rx: mpsc::UnboundedReceiver<AuditEvent>,
    state: Arc<dyn StateStore>,
    subscriptions: SubscriptionRegistry,
) {
    while let Some(event) = rx.recv().await {
        // Persist to the state store.
        if let Err(e) = state.audit_append(&event).await {
            tracing::error!(error = %e, "failed to persist audit event");
        }

        // Fan out to live audit.tail subscribers.
        let notif = JsonRpcNotification::new(
            "audit.event",
            serde_json::to_value(&event).unwrap_or(serde_json::Value::Null),
        );
        subscriptions.broadcast(&notif);
    }
    tracing::debug!("audit drain task exiting");
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::{TimeZone, Utc};
    use onesync_core::engine::observability;
    use onesync_core::ports::IdGenerator as _;
    use onesync_protocol::id::{AuditEventId, PairId};
    use onesync_protocol::primitives::Timestamp;
    use onesync_state::fakes::InMemoryStore;
    use onesync_time::fakes::TestIdGenerator;

    use super::*;
    use crate::ipc::subscriptions::SubscriptionRegistry;

    #[tokio::test]
    async fn emit_does_not_block() {
        let state = Arc::new(InMemoryStore::new());
        let reg = SubscriptionRegistry::new();
        let sink = DaemonAuditSink::new(state, reg);

        let ids = TestIdGenerator::seeded(42);
        let id: AuditEventId = ids.new_id();
        let pair_id: PairId = ids.new_id();
        // Use epoch timestamp to avoid disallowed Utc::now().
        #[allow(clippy::disallowed_methods)]
        let ts = Timestamp::from_datetime(Utc.timestamp_opt(0, 0).unwrap());

        let event = observability::cycle_started(id, ts, pair_id);
        sink.emit(event);
        // Give the drain task a moment to process.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
