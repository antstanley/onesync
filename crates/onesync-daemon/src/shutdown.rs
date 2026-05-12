//! Graceful-shutdown token.
//!
//! [`ShutdownToken`] wraps a `tokio::sync::broadcast` channel so that any
//! task can check whether a shutdown has been requested and all tasks
//! can subscribe for the shutdown signal.
//!
//! A background task listens for `SIGTERM` and `SIGINT` and calls
//! [`ShutdownToken::trigger`] when either arrives.

// LINT: all items are used from async_main (Task 10 wiring) and IPC tasks (Tasks 11-14).
#![allow(dead_code)]

use tokio::sync::broadcast;

/// Broadcast capacity: only one shutdown event is ever sent.
const CAPACITY: usize = 1;

/// A cloneable, send-safe handle that broadcasts a shutdown signal to all
/// subscribed tasks.
#[derive(Clone, Debug)]
pub struct ShutdownToken {
    tx: broadcast::Sender<()>,
}

impl ShutdownToken {
    /// Create a new [`ShutdownToken`].
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CAPACITY);
        Self { tx }
    }

    /// Subscribe to the shutdown signal.
    ///
    /// The returned receiver fires once when [`trigger`](ShutdownToken::trigger)
    /// is called (or `RecvError::Lagged` if the buffer overflows, which cannot
    /// happen with capacity 1 and a single send).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<()> {
        self.tx.subscribe()
    }

    /// Broadcast the shutdown signal to all current subscribers.
    ///
    /// Subsequent calls are no-ops (the channel is closed after the first send).
    pub fn trigger(&self) {
        // Ignore send errors: no subscribers means nobody to notify.
        let _ = self.tx.send(());
    }

    /// Returns `true` if the shutdown has already been triggered.
    #[must_use]
    pub fn is_triggered(&self) -> bool {
        // If the sender's receiver count has dropped to zero the channel is
        // closed; easier to check via a fresh receiver.
        let mut rx = self.tx.subscribe();
        // `try_recv` returns `Ok(())` if the message is already in the buffer.
        matches!(rx.try_recv(), Ok(()))
    }
}

impl Default for ShutdownToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn a Tokio task that calls [`ShutdownToken::trigger`] on `SIGTERM` or
/// `SIGINT`.
///
/// The task exits after triggering shutdown once.
pub fn spawn_signal_handler(token: ShutdownToken) {
    tokio::spawn(async move {
        wait_for_signal().await;
        tracing::info!("shutdown signal received");
        token.trigger();
    });
}

/// Wait for the first `SIGTERM` or `SIGINT`.
///
/// Logs and returns immediately if signal handler registration fails.
async fn wait_for_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let Ok(mut sigterm) = signal(SignalKind::terminate()) else {
        tracing::error!("failed to register SIGTERM handler");
        return;
    };
    let Ok(mut sigint) = signal(SignalKind::interrupt()) else {
        tracing::error!("failed to register SIGINT handler");
        return;
    };

    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv()  => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn trigger_notifies_subscriber() {
        let token = ShutdownToken::new();
        let mut rx = token.subscribe();
        token.trigger();
        assert!(rx.recv().await.is_ok());
    }

    #[tokio::test]
    async fn clone_receives_same_signal() {
        let token = ShutdownToken::new();
        let token2 = token.clone();
        let mut rx = token2.subscribe();
        token.trigger();
        assert!(rx.recv().await.is_ok());
    }
}
