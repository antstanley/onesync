//! Per-pair owning task + `mpsc` trigger channel.
//!
//! Each `Pair` has its own `mpsc` event channel and a single owning task.
//! Cross-task IPC commands coordinate via this channel, never by taking the lock
//! from outside the owning task.
//!
//! See [`docs/spec/03-sync-engine.md`](../../../../docs/spec/03-sync-engine.md) §Triggers and scheduling.

use onesync_protocol::id::PairId;
use tokio::sync::mpsc;

use crate::limits::{LOCAL_DEBOUNCE_MS, REMOTE_DEBOUNCE_MS};

/// A trigger that tells the engine to run a cycle for a pair.
#[derive(Debug, Clone)]
pub enum Trigger {
    /// A local filesystem event was observed.
    LocalEvent,
    /// A remote webhook notification arrived.
    RemoteWebhook,
    /// The scheduled poll interval fired.
    Scheduled,
    /// A CLI force-sync command was issued.
    CliForce {
        /// Whether to perform a full rescan rather than an incremental delta.
        full_scan: bool,
    },
    /// The engine is retrying a previously failed cycle.
    BackoffRetry,
    /// The pair worker should shut down cleanly.
    Shutdown,
}

impl Trigger {
    /// The debounce window in milliseconds for this trigger type, or `0` for none.
    #[must_use]
    pub const fn debounce_ms(&self) -> u64 {
        match self {
            Self::LocalEvent => LOCAL_DEBOUNCE_MS,
            Self::RemoteWebhook => REMOTE_DEBOUNCE_MS,
            Self::Scheduled | Self::CliForce { .. } | Self::BackoffRetry | Self::Shutdown => 0,
        }
    }
}

/// A handle to a pair's worker task. Callers send [`Trigger`]s through this handle.
pub struct PairWorker {
    /// The pair this worker is responsible for.
    pub pair_id: PairId,
    /// Sender half of the trigger channel.
    pub tx: mpsc::Sender<Trigger>,
}

impl PairWorker {
    /// Send a trigger to the worker task.
    ///
    /// # Errors
    ///
    /// Returns a [`mpsc::error::SendError`] if the receiver has been dropped.
    pub async fn nudge(&self, trigger: Trigger) -> Result<(), mpsc::error::SendError<Trigger>> {
        self.tx.send(trigger).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onesync_protocol::id::{Id, PairTag};
    use ulid::Ulid;

    fn pair_id() -> PairId {
        Id::<PairTag>::from_ulid(Ulid::from(42u128))
    }

    #[test]
    fn trigger_debounce_ms_local_event() {
        assert_eq!(Trigger::LocalEvent.debounce_ms(), LOCAL_DEBOUNCE_MS);
    }

    #[test]
    fn trigger_debounce_ms_remote_webhook() {
        assert_eq!(Trigger::RemoteWebhook.debounce_ms(), REMOTE_DEBOUNCE_MS);
    }

    #[test]
    fn trigger_debounce_ms_scheduled_is_zero() {
        assert_eq!(Trigger::Scheduled.debounce_ms(), 0);
    }

    #[tokio::test]
    async fn nudge_delivers_trigger_to_receiver() {
        let (tx, mut rx) = mpsc::channel::<Trigger>(8);
        let worker = PairWorker {
            pair_id: pair_id(),
            tx,
        };
        worker.nudge(Trigger::Scheduled).await.unwrap();
        let received = rx.recv().await.unwrap();
        assert!(matches!(received, Trigger::Scheduled));
    }
}
