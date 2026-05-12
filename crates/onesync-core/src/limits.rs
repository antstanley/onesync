//! Compile-time limits.
//!
//! Every limit in [`docs/spec/09-development-guidelines.md`](../../../../docs/spec/09-development-guidelines.md)
//! lives here. Units are part of the identifier; values are immediate. Operator-tunable
//! values live in `onesync_protocol::config::InstanceConfig`, never in this module.

#![allow(clippy::doc_markdown)]

/// One kibibyte (1 024 bytes).
pub const KIB: u64 = 1024;
/// One mebibyte (1 024 KiB).
pub const MIB: u64 = 1024 * KIB;
/// One gibibyte (1 024 MiB).
pub const GIB: u64 = 1024 * MIB;

// --- Sync engine ---

/// Upper bound on concurrent folder pairs per instance.
pub const MAX_PAIRS_PER_INSTANCE: usize = 16;
/// Planning truncates the op queue when this depth is reached.
pub const MAX_QUEUE_DEPTH_PER_PAIR: usize = 4_096;
/// Maximum concurrent file transfers across all pairs.
pub const MAX_CONCURRENT_TRANSFERS: usize = 4;
/// Per-pair share of the global concurrent-transfer budget.
pub const PAIR_CONCURRENT_TRANSFERS: usize = 2;
/// Maximum retry attempts per `FileOp`.
pub const RETRY_MAX_ATTEMPTS: u32 = 5;
/// Base delay in milliseconds for exponential backoff with full jitter.
pub const RETRY_BACKOFF_BASE_MS: u64 = 1_000;
/// Delta-link poll interval in milliseconds; doubled under throttling, capped at 5 min.
pub const DELTA_POLL_INTERVAL_MS: u64 = 30_000;
/// `FSEvents` coalescing window in milliseconds.
pub const LOCAL_DEBOUNCE_MS: u64 = 500;
/// Webhook coalescing window in milliseconds.
pub const REMOTE_DEBOUNCE_MS: u64 = 2_000;
/// Hard timeout in milliseconds per cycle phase.
pub const CYCLE_PHASE_TIMEOUT_MS: u64 = 60_000;
/// Tie-break window in milliseconds for conflict mtime comparison.
pub const CONFLICT_MTIME_TOLERANCE_MS: u64 = 1_000;
/// Maximum disambiguation retries when renaming a conflict loser.
pub const CONFLICT_RENAME_RETRIES: u32 = 8;

// --- Filesystem ---

/// Hard cap on a single synced file in bytes.
pub const MAX_FILE_SIZE_BYTES: u64 = 50 * GIB;
/// Maximum UTF-8 absolute path length in bytes.
pub const MAX_PATH_BYTES: usize = 1_024;
/// BLAKE3 streaming block size in bytes.
pub const HASH_BLOCK_BYTES: usize = 1024 * 1024; // 1 MiB
/// Below this size reads remain on the Tokio reactor thread.
pub const READ_INLINE_MAX: usize = 64 * 1024; // 64 KiB
/// Bounded mpsc channel depth from the filesystem watcher thread.
pub const FSEVENT_BUFFER_DEPTH: usize = 4_096;
/// Maximum directory-entry queue depth during an initial BFS scan.
pub const SCAN_QUEUE_DEPTH_MAX: usize = 65_536;
/// Backpressure limit on the concurrent-file scan stream.
pub const SCAN_INFLIGHT_MAX: usize = 1_024;
/// Minimum free disk space in bytes; downloads pause below this.
pub const DISK_FREE_MARGIN_BYTES: u64 = 2 * GIB;

// --- Microsoft Graph ---

/// Files at or below this size use a single PUT; larger files use an upload session.
pub const GRAPH_SMALL_UPLOAD_MAX_BYTES: u64 = 4 * MIB;
/// Upload-session chunk size in bytes; must be a multiple of 320 KiB per Graph requirements.
pub const SESSION_CHUNK_BYTES: u64 = 10 * MIB;
/// Token-bucket rate limit in requests per second per account.
pub const GRAPH_RPS_PER_ACCOUNT: u32 = 8;
/// Proactively refresh access tokens this many seconds before expiry.
pub const TOKEN_REFRESH_LEEWAY_S: u64 = 120;
/// Maximum wait in seconds for the OAuth loopback redirect.
pub const AUTH_LISTENER_TIMEOUT_S: u64 = 300;

// --- State store ---

/// `rusqlite` connection-pool size.
pub const STATE_POOL_SIZE: usize = 4;
/// Audit-log retention period in days.
pub const AUDIT_RETENTION_DAYS: u32 = 30;
/// Sync-run history retention period in days.
pub const RUN_HISTORY_RETENTION_DAYS: u32 = 90;
/// Resolved-conflict record retention period in days.
pub const CONFLICT_RETENTION_DAYS: u32 = 180;
/// JSONL log-file rotation threshold in bytes.
pub const LOG_ROTATE_BYTES: u64 = 32 * MIB;
/// Number of past log files retained after rotation.
pub const LOG_RETAIN_FILES: u32 = 10;

// --- IPC and lifecycle ---

/// Maximum size in bytes of a single JSON-RPC frame over the IPC socket.
pub const IPC_FRAME_MAX_BYTES: u64 = MIB;
/// Subscription liveness-ping interval in milliseconds.
pub const IPC_KEEPALIVE_MS: u64 = 30_000;
/// Dead-subscription sweep interval in milliseconds.
pub const SUB_GC_INTERVAL_MS: u64 = 60_000;
/// Seconds to poll `health.ping` after install before declaring failure.
pub const INSTALL_TIMEOUT_S: u64 = 60;
/// Graceful-shutdown drain timeout in seconds.
pub const SHUTDOWN_DRAIN_TIMEOUT_S: u64 = 30;
/// Upgrade-handoff drain timeout in seconds.
pub const UPGRADE_DRAIN_TIMEOUT_S: u64 = 30;
/// Maximum tolerated clock skew in seconds between daemon and remote.
pub const MAX_CLOCK_SKEW_S: i64 = 600;

// ── Tokio runtime ────────────────────────────────────────────────────────────

/// Maximum Tokio worker threads: `min(available_parallelism, 4)`.
///
/// This is a runtime-derived value and cannot be a `const`. Keeps the daemon
/// from monopolising the CPU on large-core machines while still saturating a
/// typical 4-core developer laptop.
#[must_use]
pub fn max_runtime_workers() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get().min(4))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_have_documented_values() {
        // Cross-check a representative sample against docs/spec/09-development-guidelines.md.
        assert_eq!(MAX_PAIRS_PER_INSTANCE, 16);
        assert_eq!(MAX_QUEUE_DEPTH_PER_PAIR, 4096);
        assert_eq!(MAX_FILE_SIZE_BYTES, 50 * GIB);
        assert_eq!(GRAPH_SMALL_UPLOAD_MAX_BYTES, 4 * MIB);
        assert_eq!(SESSION_CHUNK_BYTES % (320 * KIB), 0);
        assert_eq!(IPC_FRAME_MAX_BYTES, MIB);
        assert_eq!(AUDIT_RETENTION_DAYS, 30);
    }

    #[test]
    fn unit_suffix_constants() {
        assert_eq!(KIB, 1024);
        assert_eq!(MIB, 1024 * 1024);
        assert_eq!(GIB, 1024 * 1024 * 1024);
    }

    #[test]
    fn max_runtime_workers_is_in_expected_range() {
        let w = max_runtime_workers();
        assert!(w >= 1, "must have at least 1 worker");
        assert!(w <= 4, "must be capped at 4 workers, got {w}");
    }
}
