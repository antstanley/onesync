//! `FSEvents` watcher wrapping `notify`.
//!
//! # Rename handling
//!
//! `notify` 6.x on macOS coalesces `FSEvents` rename pairs into a single
//! [`notify::Event`] with `EventKind::Modify(ModifyKind::Name(RenameMode::Both))`
//! and two paths in `event.paths`. When only one path is present (the `From` or
//! `To` half arrived without its sibling) we fall back to emitting a `Modified`
//! event. Engine-side reconciliation can re-pair such events by content hash.

use std::path::{Path, PathBuf};

use notify::{Config, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};
use tokio::sync::mpsc;

use onesync_core::limits::FSEVENT_BUFFER_DEPTH;

use crate::error::LocalFsAdapterError;

/// Filesystem event observed under a watched pair root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalEvent {
    /// New file or directory appeared.
    Created(PathBuf),
    /// Existing file's contents/metadata changed.
    Modified(PathBuf),
    /// File or directory removed.
    Deleted(PathBuf),
    /// File or directory renamed. `from` and `to` are both present.
    Renamed {
        /// Old path (before rename).
        from: PathBuf,
        /// New path (after rename).
        to: PathBuf,
    },
    /// The watcher's internal buffer overflowed; the engine should run a full re-scan.
    Overflow,
    /// The watched volume was unmounted; the watcher stream ends after this.
    Unmounted,
}

/// Watcher handle. Drop to stop watching.
pub struct Watcher {
    _watcher: RecommendedWatcher,
    rx: mpsc::Receiver<LocalEvent>,
}

impl Watcher {
    /// Receive the next event. Returns `None` when the watcher has stopped (e.g. after
    /// `Unmounted` or when the underlying channel is closed).
    pub async fn recv(&mut self) -> Option<LocalEvent> {
        self.rx.recv().await
    }

    /// Non-blocking poll, useful in tests.
    pub fn try_recv(&mut self) -> Option<LocalEvent> {
        self.rx.try_recv().ok()
    }
}

/// Begin watching `root` recursively.
///
/// The returned `Watcher` produces a stream of `LocalEvent`s; when the underlying
/// `notify` channel is full we emit a single `Overflow` event and drop further
/// events until the consumer drains.
///
/// # Errors
/// Returns `LocalFsAdapterError::Io` if the underlying watcher cannot be constructed
/// or fails to register the path.
pub fn watch(root: &Path) -> Result<Watcher, LocalFsAdapterError> {
    let (tx, rx) = mpsc::channel::<LocalEvent>(FSEVENT_BUFFER_DEPTH);

    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            let local = res.map_or(Some(LocalEvent::Overflow), |event| translate(&event));
            if let Some(evt) = local
                && tx.try_send(evt).is_err()
            {
                // Channel full or closed — best-effort signal overflow.
                let _ = tx.try_send(LocalEvent::Overflow);
            }
        },
        Config::default(),
    )
    .map_err(|e| LocalFsAdapterError::Io(format!("watcher init: {e}")))?;

    watcher
        .watch(root, RecursiveMode::Recursive)
        .map_err(|e| LocalFsAdapterError::Io(format!("watch register: {e}")))?;

    Ok(Watcher {
        _watcher: watcher,
        rx,
    })
}

fn translate(event: &notify::Event) -> Option<LocalEvent> {
    let path = event.paths.first().cloned()?;
    match event.kind {
        EventKind::Create(_) => Some(LocalEvent::Created(path)),
        EventKind::Modify(notify::event::ModifyKind::Name(_)) => {
            // Rename: notify emits two events with `Name(From)` and `Name(To)`. We
            // surface the second one (with a path) as `Renamed { from = first, to = second }`
            // only if `event.paths` has two entries; otherwise fall through to Modified.
            if event.paths.len() >= 2 {
                Some(LocalEvent::Renamed {
                    from: event.paths[0].clone(),
                    to: event.paths[1].clone(),
                })
            } else {
                Some(LocalEvent::Modified(path))
            }
        }
        EventKind::Modify(_) => Some(LocalEvent::Modified(path)),
        EventKind::Remove(_) => Some(LocalEvent::Deleted(path)),
        EventKind::Other => {
            // notify on macOS emits Other for some FSEvents flags; skip unless they
            // bear the `kFSEventStreamEventFlagMustScanSubDirs` marker, which `notify`
            // currently doesn't surface as a distinct kind. Treat as no-op for now.
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::time::timeout;

    fn write_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).expect("create");
        f.write_all(content).expect("write");
        p
    }

    async fn next_event(watcher: &mut Watcher, max_ms: u64) -> Option<LocalEvent> {
        timeout(Duration::from_millis(max_ms), watcher.recv())
            .await
            .ok()
            .flatten()
    }

    #[tokio::test]
    async fn watcher_observes_a_created_file() {
        let tmp = TempDir::new().expect("tmpdir");
        let mut w = watch(tmp.path()).expect("watch");

        // small settle so the watcher is fully registered before we cause events.
        tokio::time::sleep(Duration::from_millis(100)).await;
        write_file(tmp.path(), "new.txt", b"hi");

        let evt = next_event(&mut w, 5_000).await.expect("event");
        #[allow(clippy::panic)]
        match evt {
            LocalEvent::Created(_) | LocalEvent::Modified(_) => {} // FSEvents may report either
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn watcher_observes_a_deleted_file() {
        let tmp = TempDir::new().expect("tmpdir");
        let p = write_file(tmp.path(), "doomed.txt", b"bye");
        let mut w = watch(tmp.path()).expect("watch");

        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::remove_file(&p).expect("rm");

        // Drain events for up to 5s, looking for a Deleted matching our path.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut saw_delete = false;
        while std::time::Instant::now() < deadline {
            let Some(evt) = next_event(&mut w, 500).await else {
                continue;
            };
            if matches!(evt, LocalEvent::Deleted(_)) {
                saw_delete = true;
                break;
            }
        }
        assert!(saw_delete, "expected to see a Deleted event");
    }

    #[tokio::test]
    async fn watcher_can_be_dropped_without_panic() {
        let tmp = TempDir::new().expect("tmpdir");
        let w = watch(tmp.path()).expect("watch");
        drop(w);
        // Sleep briefly to let the underlying FSEvents thread shut down.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
