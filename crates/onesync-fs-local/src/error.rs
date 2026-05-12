//! Internal error type, mapped to `LocalFsError` at the port boundary.

/// Errors raised by `onesync-fs-local` internals.
#[derive(Debug, thiserror::Error)]
pub enum LocalFsAdapterError {
    /// Path is invalid (NFC violation, embedded NUL, escapes pair root, etc.).
    #[error("invalid path: {reason}")]
    InvalidPath {
        /// Human-readable reason the path was rejected.
        reason: String,
    },
    /// The hashed file's mtime changed between open and final read.
    #[error("raced (mtime changed during hash)")]
    Raced,
    /// Rename between volumes used the copy+delete fallback.
    #[error("cross-volume rename ({method})")]
    CrossVolumeRename {
        /// The fallback method name.
        method: &'static str,
    },
    /// Generic I/O error from `std::fs`.
    #[error("io: {0}")]
    Io(String),
}

impl From<std::io::Error> for LocalFsAdapterError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}
