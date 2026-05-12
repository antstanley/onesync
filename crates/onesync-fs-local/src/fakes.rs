//! In-memory `LocalFs` implementation for tests.

#![cfg(any(test, feature = "fakes"))]
#![allow(clippy::expect_used)]
// LINT: this is a test-double surface; mutex-poison expects are the standard pattern.
#![allow(clippy::disallowed_methods)]
// LINT: test-only fake; no Clock injected at this layer.
#![allow(clippy::significant_drop_tightening)]
// LINT: test-double surface; early drop adds noise without benefit here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::mpsc;

use onesync_core::ports::{
    LocalEventDto, LocalEventStream, LocalFs, LocalFsError, LocalReadStream, LocalScanStream,
    LocalWriteStream,
};
use onesync_protocol::{
    enums::FileKind,
    file_side::FileSide,
    path::AbsPath,
    primitives::{ContentHash, Timestamp},
};

/// In-memory representation of a fake file.
#[derive(Debug, Clone)]
struct FakeFile {
    kind: FileKind,
    bytes: Vec<u8>,
    mtime: Timestamp,
}

/// In-memory `LocalFs` for use in engine tests.
#[derive(Debug, Default)]
pub struct InMemoryLocalFs {
    files: Mutex<HashMap<PathBuf, FakeFile>>,
    /// Sender used by `inject_event` to push events into the watcher stream(s).
    watcher_tx: Mutex<Option<mpsc::Sender<LocalEventDto>>>,
}

impl InMemoryLocalFs {
    /// New empty fake.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Pre-populate a file with the given content. Test-only helper.
    pub fn seed_file(&self, path: &AbsPath, bytes: &[u8], mtime: Timestamp) {
        let mut guard = self.files.lock().expect("files lock");
        guard.insert(
            PathBuf::from(path.as_str()),
            FakeFile {
                kind: FileKind::File,
                bytes: bytes.to_vec(),
                mtime,
            },
        );
    }

    /// Pre-create a directory entry. Test-only helper.
    pub fn seed_dir(&self, path: &AbsPath, mtime: Timestamp) {
        let mut guard = self.files.lock().expect("files lock");
        guard.insert(
            PathBuf::from(path.as_str()),
            FakeFile {
                kind: FileKind::Directory,
                bytes: Vec::new(),
                mtime,
            },
        );
    }

    /// Push a synthetic event into any active watcher stream. Test-only helper.
    pub async fn inject_event(&self, event: LocalEventDto) {
        let tx = {
            let guard = self.watcher_tx.lock().expect("watcher_tx lock");
            guard.clone()
        };
        if let Some(tx) = tx {
            let _ = tx.send(event).await;
        }
    }
}

#[async_trait]
impl LocalFs for InMemoryLocalFs {
    async fn scan(&self, root: &AbsPath) -> Result<LocalScanStream, LocalFsError> {
        let root_pb = PathBuf::from(root.as_str());
        let entries: Vec<(PathBuf, FileSide)> = {
            let guard = self.files.lock().expect("files lock");
            guard
                .iter()
                .filter(|(p, _)| p.starts_with(&root_pb) && *p != &root_pb)
                .map(|(p, f)| {
                    let side = FileSide {
                        kind: f.kind,
                        size_bytes: f.bytes.len() as u64,
                        content_hash: None,
                        mtime: f.mtime,
                        etag: None,
                        remote_item_id: None,
                    };
                    (p.clone(), side)
                })
                .collect()
        };
        Ok(LocalScanStream(entries))
    }

    async fn read(&self, path: &AbsPath) -> Result<LocalReadStream, LocalFsError> {
        let pb = PathBuf::from(path.as_str());
        let bytes = {
            let guard = self.files.lock().expect("files lock");
            let file = guard
                .get(&pb)
                .ok_or_else(|| LocalFsError::NotFound(path.as_str().to_owned()))?;
            if file.kind != FileKind::File {
                return Err(LocalFsError::InvalidPath {
                    reason: "not a file".into(),
                });
            }
            file.bytes.clone()
        };
        Ok(LocalReadStream(bytes))
    }

    async fn write_atomic(
        &self,
        path: &AbsPath,
        stream: LocalWriteStream,
    ) -> Result<FileSide, LocalFsError> {
        let pb = PathBuf::from(path.as_str());
        let bytes = stream.0;
        // LINT: fake-only wall-clock anchor; tests should inject a Clock if they care.
        let mtime = Timestamp::from_datetime(chrono::Utc::now());
        let digest: [u8; 32] = *blake3::hash(&bytes).as_bytes();
        let size = bytes.len() as u64;

        {
            let mut guard = self.files.lock().expect("files lock");
            guard.insert(
                pb,
                FakeFile {
                    kind: FileKind::File,
                    bytes,
                    mtime,
                },
            );
        }
        Ok(FileSide {
            kind: FileKind::File,
            size_bytes: size,
            content_hash: Some(ContentHash::from_bytes(digest)),
            mtime,
            etag: None,
            remote_item_id: None,
        })
    }

    async fn rename(&self, from: &AbsPath, to: &AbsPath) -> Result<(), LocalFsError> {
        let f = PathBuf::from(from.as_str());
        let t = PathBuf::from(to.as_str());
        let mut guard = self.files.lock().expect("files lock");
        let Some(file) = guard.remove(&f) else {
            return Err(LocalFsError::NotFound(from.as_str().to_owned()));
        };
        guard.insert(t, file);
        drop(guard);
        Ok(())
    }

    async fn delete(&self, path: &AbsPath) -> Result<(), LocalFsError> {
        let pb = PathBuf::from(path.as_str());
        let removed = {
            let mut guard = self.files.lock().expect("files lock");
            guard.remove(&pb)
        };
        if removed.is_none() {
            return Err(LocalFsError::NotFound(path.as_str().to_owned()));
        }
        Ok(())
    }

    async fn mkdir_p(&self, path: &AbsPath) -> Result<(), LocalFsError> {
        let pb = PathBuf::from(path.as_str());
        // LINT: fake-only wall-clock anchor; tests should inject a Clock if they care.
        let mtime = Timestamp::from_datetime(chrono::Utc::now());
        {
            let mut guard = self.files.lock().expect("files lock");
            guard.entry(pb).or_insert(FakeFile {
                kind: FileKind::Directory,
                bytes: Vec::new(),
                mtime,
            });
        }
        Ok(())
    }

    async fn watch(&self, _root: &AbsPath) -> Result<LocalEventStream, LocalFsError> {
        let (tx, rx) = mpsc::channel::<LocalEventDto>(1024);
        // Store the sender so inject_event can push events.
        *self.watcher_tx.lock().expect("watcher_tx lock") = Some(tx);
        Ok(LocalEventStream(rx))
    }

    async fn hash(&self, path: &AbsPath) -> Result<ContentHash, LocalFsError> {
        let pb = PathBuf::from(path.as_str());
        let digest: [u8; 32] = {
            let guard = self.files.lock().expect("files lock");
            let file = guard
                .get(&pb)
                .ok_or_else(|| LocalFsError::NotFound(path.as_str().to_owned()))?;
            if file.kind != FileKind::File {
                return Err(LocalFsError::InvalidPath {
                    reason: "not a file".into(),
                });
            }
            *blake3::hash(&file.bytes).as_bytes()
        };
        Ok(ContentHash::from_bytes(digest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onesync_core::ports::LocalFs;

    fn abs(s: &str) -> AbsPath {
        s.parse::<AbsPath>().expect("absolutepath")
    }

    fn ts() -> Timestamp {
        Timestamp::from_datetime(
            chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 5, 12, 10, 0, 0).unwrap(),
        )
    }

    #[tokio::test]
    async fn write_then_read_round_trips() {
        let fs = InMemoryLocalFs::new();
        let p = abs("/root/file.bin");
        let side = fs
            .write_atomic(&p, LocalWriteStream(b"hello".to_vec()))
            .await
            .expect("write");
        assert_eq!(side.size_bytes, 5);
        let r = fs.read(&p).await.expect("read");
        assert_eq!(r.0, b"hello");
    }

    #[tokio::test]
    async fn scan_returns_seeded_entries_under_root() {
        let fs = InMemoryLocalFs::new();
        fs.seed_dir(&abs("/root"), ts());
        fs.seed_file(&abs("/root/a.txt"), b"a", ts());
        fs.seed_file(&abs("/elsewhere/b.txt"), b"b", ts());

        let scan = fs.scan(&abs("/root")).await.expect("scan");
        assert_eq!(
            scan.0.len(),
            1,
            "only files under /root, excluding the root itself"
        );
        assert!(scan.0[0].0.ends_with("a.txt"));
    }

    #[tokio::test]
    async fn rename_then_old_path_is_not_found() {
        let fs = InMemoryLocalFs::new();
        let from = abs("/root/a.txt");
        let to = abs("/root/b.txt");
        let _ = fs
            .write_atomic(&from, LocalWriteStream(b"x".to_vec()))
            .await
            .expect("write");
        fs.rename(&from, &to).await.expect("rename");
        let read_result = fs.read(&from).await;
        assert!(
            matches!(read_result, Err(LocalFsError::NotFound(_))),
            "expected NotFound, got something else"
        );
        let r = fs.read(&to).await.expect("to present");
        assert_eq!(r.0, b"x");
    }

    #[tokio::test]
    async fn inject_event_delivers_to_watcher() {
        let fs = InMemoryLocalFs::new();
        let mut stream = fs.watch(&abs("/root")).await.expect("watch");
        fs.inject_event(LocalEventDto::Created(PathBuf::from("/root/new.txt")))
            .await;
        let evt = stream.0.recv().await.expect("event");
        assert!(matches!(evt, LocalEventDto::Created(_)));
    }
}
