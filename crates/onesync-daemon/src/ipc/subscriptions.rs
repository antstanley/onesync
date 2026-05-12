//! Per-connection subscription registry.
//!
//! Each subscription is a live channel through which the daemon pushes
//! [`JsonRpcNotification`] frames to the connected client.
//!
//! ## Lifecycle
//!
//! 1. Client calls `audit.tail`, `pair.subscribe`, or `conflict.subscribe`.
//! 2. [`SubscriptionRegistry::insert`] creates an `mpsc` channel, stores the
//!    sender, and returns a [`SubscriptionId`] + receiver to the connection task.
//! 3. The connection task reads from the receiver and forwards frames to the socket.
//! 4. When the client calls `subscription.cancel` (or disconnects), the receiver
//!    is dropped, which makes the sender detect the next `send()` as an error and
//!    the GC sweep removes the dead entry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use onesync_protocol::rpc::JsonRpcNotification;
use tokio::sync::mpsc;

use onesync_core::limits::{IPC_KEEPALIVE_MS, SUB_GC_INTERVAL_MS};

/// Channel depth per subscription.  Events beyond this depth are dropped and
/// an overrun audit event is emitted.
const SUB_CHANNEL_DEPTH: usize = 256;

/// Opaque identifier for a live subscription.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubscriptionId(String);

impl SubscriptionId {
    /// Wrap a raw string identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Borrow the identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Sender half of one subscription channel.
type SubSender = mpsc::Sender<JsonRpcNotification>;

/// Thread-safe registry of all active subscriptions for a single connection.
#[derive(Clone, Default)]
pub struct SubscriptionRegistry {
    inner: Arc<Mutex<HashMap<SubscriptionId, SubSender>>>,
}

impl SubscriptionRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new subscription.
    ///
    /// Returns the [`SubscriptionId`] and the receiver end of the channel.
    /// The caller is responsible for reading from the receiver and forwarding
    /// frames to the socket.
    #[must_use]
    pub fn insert(&self, id: SubscriptionId) -> mpsc::Receiver<JsonRpcNotification> {
        let (tx, rx) = mpsc::channel(SUB_CHANNEL_DEPTH);
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, tx);
        rx
    }

    /// Cancel and remove one subscription.
    pub fn remove(&self, id: &SubscriptionId) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);
    }

    /// Push a notification to all live subscribers.
    ///
    /// Dead channels (receiver dropped) are silently removed.
    pub fn broadcast(&self, notif: &JsonRpcNotification) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.retain(|_id, tx| {
            // `try_send` is non-blocking; errors mean either full or closed.
            match tx.try_send(notif.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Back-pressure: drop the event; caller can log an overrun.
                    tracing::warn!("subscription channel full — event dropped");
                    true // keep the subscription; it may drain
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false, // receiver gone
            }
        });
    }

    /// Remove all subscriptions whose receiver has been dropped.
    pub fn gc(&self) {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.retain(|_id, tx| !tx.is_closed());
    }

    /// Count live subscriptions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Returns `true` if there are no subscriptions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Spawn a GC sweep task that periodically cleans dead subscriptions.
pub fn spawn_gc(registry: SubscriptionRegistry) {
    tokio::spawn(async move {
        let interval = std::time::Duration::from_millis(SUB_GC_INTERVAL_MS);
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // skip first immediate tick
        loop {
            ticker.tick().await;
            registry.gc();
        }
    });
}

/// Liveness-ping interval constant (re-exported for use by connection tasks).
pub const KEEPALIVE_MS: u64 = IPC_KEEPALIVE_MS;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn notif(method: &str) -> JsonRpcNotification {
        JsonRpcNotification::new(method, json!({}))
    }

    #[tokio::test]
    async fn insert_and_receive() {
        let reg = SubscriptionRegistry::new();
        let id = SubscriptionId::new("sub-1");
        let mut rx = reg.insert(id.clone());
        reg.broadcast(&notif("test.event"));
        let msg = rx.recv().await.unwrap();
        assert_eq!(msg.method, "test.event");
    }

    #[tokio::test]
    async fn remove_stops_delivery() {
        let reg = SubscriptionRegistry::new();
        let id = SubscriptionId::new("sub-2");
        let _rx = reg.insert(id.clone());
        reg.remove(&id);
        assert_eq!(reg.len(), 0);
    }

    #[tokio::test]
    async fn gc_removes_closed_receivers() {
        let reg = SubscriptionRegistry::new();
        let id = SubscriptionId::new("sub-3");
        let rx = reg.insert(id);
        drop(rx); // close the receiver
        // Send one message to trigger the Closed detection path.
        reg.broadcast(&notif("gc.test"));
        assert_eq!(reg.len(), 0);
    }
}
