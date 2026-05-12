//! Scheduler: `PairWorker` and `Trigger` types that drive periodic sync cycles.
//!
//! A `PairWorker` owns one pair's state (id, root, cursor) and drives cycles
//! via an internal `Trigger` channel. Populated fully in Task 5.

use onesync_protocol::{id::PairId, primitives::DeltaCursor};
use tokio::sync::mpsc;

/// What caused a sync cycle to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// Timer fired at the configured interval.
    Scheduled,
    /// A local filesystem event was coalesced.
    LocalEvent,
    /// A remote webhook or poll detected a change.
    RemoteWebhook,
    /// User or CLI requested an immediate sync.
    CliForce,
    /// A previously-failed operation is being retried after backoff.
    BackoffRetry,
}

/// Commands the control plane sends to a `PairWorker`.
#[derive(Debug)]
pub enum WorkerCommand {
    /// Trigger a sync cycle for the given reason.
    Sync(Trigger),
    /// Pause further cycles until `Resume` is received.
    Pause,
    /// Resume a paused worker.
    Resume,
    /// Shut the worker down cleanly.
    Shutdown,
}

/// Handle returned when spawning a `PairWorker`.
pub struct PairWorkerHandle {
    /// The pair this worker owns.
    pub pair_id: PairId,
    /// Channel used to send commands to the worker task.
    pub tx: mpsc::Sender<WorkerCommand>,
}

impl PairWorkerHandle {
    /// Send a sync trigger to the worker.
    ///
    /// Returns `Err` if the worker has already shut down.
    pub async fn trigger(
        &self,
        reason: Trigger,
    ) -> Result<(), mpsc::error::SendError<WorkerCommand>> {
        self.tx.send(WorkerCommand::Sync(reason)).await
    }

    /// Request the worker to shut down.
    ///
    /// Returns `Err` if the channel is already closed.
    pub async fn shutdown(&self) -> Result<(), mpsc::error::SendError<WorkerCommand>> {
        self.tx.send(WorkerCommand::Shutdown).await
    }
}

/// Per-pair mutable state carried by the worker task.
#[derive(Debug, Default)]
pub struct PairState {
    /// Most recent delta cursor; `None` triggers a full rescan.
    pub delta_cursor: Option<DeltaCursor>,
    /// Whether this worker is currently paused.
    pub paused: bool,
}

/// Spawn a `PairWorker` task for `pair_id`.
///
/// Returns a [`PairWorkerHandle`] the caller uses to send commands.
/// The worker loop body is populated in Task 5.
#[must_use]
pub fn spawn_pair_worker(pair_id: PairId) -> PairWorkerHandle {
    let (tx, mut rx) = mpsc::channel::<WorkerCommand>(32);
    // Stub task: drain commands until the sender is dropped or Shutdown arrives.
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            if matches!(cmd, WorkerCommand::Shutdown) {
                break;
            }
        }
    });
    PairWorkerHandle { pair_id, tx }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ulid::Ulid;

    fn pair() -> PairId {
        // LINT: Ulid::new() allowed in tests.
        #[allow(clippy::disallowed_methods)]
        PairId::from_ulid(Ulid::new())
    }

    #[tokio::test]
    async fn pair_worker_handle_can_receive_shutdown() {
        let handle = spawn_pair_worker(pair());
        // Channel is buffered; send should not block.
        handle.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn trigger_sync_sends_scheduled() {
        let handle = spawn_pair_worker(pair());
        handle.trigger(Trigger::Scheduled).await.unwrap();
    }
}
