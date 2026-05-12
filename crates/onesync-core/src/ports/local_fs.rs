//! `LocalFs` port: the macOS filesystem surface the engine drives.

use async_trait::async_trait;
use onesync_protocol::{file_side::FileSide, path::AbsPath, primitives::ContentHash};

/// Errors returned by `LocalFs` operations.
#[derive(Debug, thiserror::Error)]
pub enum LocalFsError {
    /// Path does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// Insufficient permissions to read/write the path.
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// The volume containing the path is no longer mounted.
    #[error("not mounted: {0}")]
    NotMounted(String),
    /// Out of space on the target volume.
    #[error("disk full")]
    DiskFull,
    /// Per-user filesystem quota exceeded.
    #[error("quota exceeded")]
    QuotaExceeded,
    /// Another instance of the daemon is already running.
    #[error("already running: {0}")]
    AlreadyRunning(String),
    /// A rename was requested between paths on different volumes; degraded to copy+delete.
    #[error("cross-volume rename ({method})")]
    CrossVolumeRename {
        /// The fallback method used (e.g. "copy+delete").
        method: &'static str,
    },
    /// The given path failed validation (non-NFC, embedded NUL, etc.).
    #[error("invalid path: {reason}")]
    InvalidPath {
        /// Human-readable reason the path was rejected.
        reason: String,
    },
    /// The file's mtime changed while we were operating on it.
    #[error("raced (mtime changed under us)")]
    Raced,
    /// Generic I/O error.
    #[error("io: {0}")]
    Io(String),
}

/// Placeholder stream type for `FSEvents` output; concrete implementation lands in `onesync-fs-local` (M2).
pub struct LocalEventStream;
/// Placeholder stream type for directory scans; concrete implementation lands in `onesync-fs-local` (M2).
pub struct LocalScanStream;
/// Placeholder stream type for file reads; concrete implementation lands in `onesync-fs-local` (M2).
pub struct LocalReadStream;
/// Placeholder stream type for file writes; concrete implementation lands in `onesync-fs-local` (M2).
pub struct LocalWriteStream;

/// The macOS filesystem surface the engine drives.
#[async_trait]
pub trait LocalFs: Send + Sync {
    /// Begin a recursive scan of `root`, returning a stream of file metadata.
    async fn scan(&self, root: &AbsPath) -> Result<LocalScanStream, LocalFsError>;
    /// Open a streaming read for an existing file.
    async fn read(&self, path: &AbsPath) -> Result<LocalReadStream, LocalFsError>;
    /// Write the contents of `stream` to `path` atomically (temp + rename + fsync).
    async fn write_atomic(
        &self,
        path: &AbsPath,
        stream: LocalWriteStream,
    ) -> Result<FileSide, LocalFsError>;
    /// Rename `from` to `to`. Degrades to copy+delete if the paths are on different volumes.
    async fn rename(&self, from: &AbsPath, to: &AbsPath) -> Result<(), LocalFsError>;
    /// Delete a single file or empty directory.
    async fn delete(&self, path: &AbsPath) -> Result<(), LocalFsError>;
    /// Create the directory at `path`, including any missing parents.
    async fn mkdir_p(&self, path: &AbsPath) -> Result<(), LocalFsError>;
    /// Open an `FSEvents` watcher rooted at `root`.
    async fn watch(&self, root: &AbsPath) -> Result<LocalEventStream, LocalFsError>;
    /// Compute the BLAKE3 content hash of a file at `path`.
    async fn hash(&self, path: &AbsPath) -> Result<ContentHash, LocalFsError>;
}
