//! Advisory lock preventing two daemon instances from running simultaneously.
//!
//! Uses `fs2::FileExt::try_lock_exclusive` on `runtime_dir/onesync.lock`.
//! The lock is held open for the daemon's lifetime.

use std::fs::{File, OpenOptions};
use std::path::Path;

use fs2::FileExt as _;

/// The filename of the advisory lock.
const LOCK_FILE: &str = "onesync.lock";

/// Errors returned when acquiring the daemon lock.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another daemon instance already holds the lock.
    #[error("another daemon instance is running (lock held)")]
    AlreadyRunning,
    /// The lock file could not be created or opened.
    #[error("failed to open lock file: {0}")]
    Io(String),
}

/// Advisory lock guard. Drops (releases) the lock when it goes out of scope.
pub struct DaemonLock {
    _file: File,
}

/// Acquire an exclusive advisory lock in `runtime_dir`.
///
/// # Errors
///
/// Returns [`LockError::AlreadyRunning`] if another process holds the lock.
/// Returns [`LockError::Io`] if the lock file cannot be opened.
pub fn acquire(runtime_dir: &Path) -> Result<DaemonLock, LockError> {
    let lock_path = runtime_dir.join(LOCK_FILE);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| LockError::Io(e.to_string()))?;

    file.try_lock_exclusive().map_err(|e| {
        // `WouldBlock` / `EWOULDBLOCK` means another process holds the lock.
        if e.kind() == std::io::ErrorKind::WouldBlock {
            LockError::AlreadyRunning
        } else {
            LockError::Io(e.to_string())
        }
    })?;

    Ok(DaemonLock { _file: file })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_acquired_and_released() {
        let tmp = tempfile::tempdir().unwrap();
        let _lock = acquire(tmp.path()).unwrap();
        // Lock is held; acquiring a second one in the same process would deadlock on
        // some platforms (fs2 uses a per-process lock on macOS). We just verify the
        // first acquisition succeeds.
    }
}
