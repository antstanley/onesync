//! `LocalFsAdapter` — concrete `LocalFs` port implementation.

use std::path::PathBuf;

use async_trait::async_trait;
use onesync_core::ports::{
    LocalEventDto, LocalEventStream, LocalFs, LocalFsError, LocalReadStream, LocalScanStream,
    LocalWriteStream,
};
use onesync_protocol::{file_side::FileSide, path::AbsPath, primitives::ContentHash};

use crate::error::LocalFsAdapterError;
use crate::{hash, ops, scan, watcher, write};

/// `LocalFs` adapter backed by the real macOS filesystem.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalFsAdapter;

fn map_err(e: LocalFsAdapterError) -> LocalFsError {
    match e {
        LocalFsAdapterError::InvalidPath { reason } => LocalFsError::InvalidPath { reason },
        LocalFsAdapterError::Raced => LocalFsError::Raced,
        LocalFsAdapterError::CrossVolumeRename { method } => {
            LocalFsError::CrossVolumeRename { method }
        }
        LocalFsAdapterError::Io(s) => LocalFsError::Io(s),
    }
}

fn map_io(e: &std::io::Error) -> LocalFsError {
    LocalFsError::Io(e.to_string())
}

#[async_trait]
impl LocalFs for LocalFsAdapter {
    async fn scan(&self, root: &AbsPath) -> Result<LocalScanStream, LocalFsError> {
        let root_buf = PathBuf::from(root.as_str());
        let metas = tokio::task::spawn_blocking(move || scan::scan(&root_buf))
            .await
            .map_err(|e| LocalFsError::Io(format!("join: {e}")))?
            .map_err(map_err)?;

        // Convert LocalFileMeta -> (PathBuf, FileSide).
        // content_hash is left None at scan time; the engine calls `hash` lazily.
        let out: Vec<(PathBuf, FileSide)> = metas
            .into_iter()
            .map(|m| {
                let side = FileSide {
                    kind: m.kind,
                    size_bytes: m.size_bytes,
                    content_hash: None,
                    mtime: m.mtime,
                    etag: None,
                    remote_item_id: None,
                };
                (m.path, side)
            })
            .collect();

        Ok(LocalScanStream(out))
    }

    async fn read(&self, path: &AbsPath) -> Result<LocalReadStream, LocalFsError> {
        let pb = PathBuf::from(path.as_str());
        let bytes = tokio::task::spawn_blocking(move || std::fs::read(&pb))
            .await
            .map_err(|e| LocalFsError::Io(format!("join: {e}")))?
            .map_err(|e| map_io(&e))?;
        Ok(LocalReadStream(bytes))
    }

    async fn write_atomic(
        &self,
        path: &AbsPath,
        stream: LocalWriteStream,
    ) -> Result<FileSide, LocalFsError> {
        let pb = PathBuf::from(path.as_str());
        let bytes = stream.0;
        tokio::task::spawn_blocking(move || write::write_atomic(&pb, &bytes))
            .await
            .map_err(|e| LocalFsError::Io(format!("join: {e}")))?
            .map_err(map_err)
    }

    async fn rename(&self, from: &AbsPath, to: &AbsPath) -> Result<(), LocalFsError> {
        let f = PathBuf::from(from.as_str());
        let t = PathBuf::from(to.as_str());
        tokio::task::spawn_blocking(move || ops::rename(&f, &t))
            .await
            .map_err(|e| LocalFsError::Io(format!("join: {e}")))?
            .map_err(map_err)
    }

    async fn delete(&self, path: &AbsPath) -> Result<(), LocalFsError> {
        let pb = PathBuf::from(path.as_str());
        tokio::task::spawn_blocking(move || ops::delete(&pb))
            .await
            .map_err(|e| LocalFsError::Io(format!("join: {e}")))?
            .map_err(map_err)
    }

    async fn mkdir_p(&self, path: &AbsPath) -> Result<(), LocalFsError> {
        let pb = PathBuf::from(path.as_str());
        tokio::task::spawn_blocking(move || ops::mkdir_p(&pb))
            .await
            .map_err(|e| LocalFsError::Io(format!("join: {e}")))?
            .map_err(map_err)
    }

    async fn watch(&self, root: &AbsPath) -> Result<LocalEventStream, LocalFsError> {
        let pb = PathBuf::from(root.as_str());
        let mut w = watcher::watch(&pb).map_err(map_err)?;

        // Translate inner `LocalEvent` variants to port-level `LocalEventDto`s via a
        // dedicated Tokio task, bridging the two channel types.
        let (tx, rx) = tokio::sync::mpsc::channel::<LocalEventDto>(1024);
        tokio::spawn(async move {
            while let Some(evt) = w.recv().await {
                let dto = match evt {
                    crate::watcher::LocalEvent::Created(p) => LocalEventDto::Created(p),
                    crate::watcher::LocalEvent::Modified(p) => LocalEventDto::Modified(p),
                    crate::watcher::LocalEvent::Deleted(p) => LocalEventDto::Deleted(p),
                    crate::watcher::LocalEvent::Renamed { from, to } => {
                        LocalEventDto::Renamed { from, to }
                    }
                    crate::watcher::LocalEvent::Overflow => LocalEventDto::Overflow,
                    crate::watcher::LocalEvent::Unmounted => LocalEventDto::Unmounted,
                };
                if tx.send(dto).await.is_err() {
                    break;
                }
            }
        });
        Ok(LocalEventStream(rx))
    }

    async fn hash(&self, path: &AbsPath) -> Result<ContentHash, LocalFsError> {
        let pb = PathBuf::from(path.as_str());
        tokio::task::spawn_blocking(move || hash::hash(&pb))
            .await
            .map_err(|e| LocalFsError::Io(format!("join: {e}")))?
            .map_err(map_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onesync_core::ports::LocalFs;
    use tempfile::TempDir;

    fn abs(p: &std::path::Path) -> AbsPath {
        p.to_str().expect("utf8").parse().expect("absolutepath")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adapter_round_trips_a_scan_write_read_delete_pipeline() {
        let tmp = TempDir::new().expect("tmpdir");
        let adapter = LocalFsAdapter;

        // mkdir_p
        let nested = tmp.path().join("a/b");
        adapter.mkdir_p(&abs(&nested)).await.expect("mkdir_p");

        // write_atomic
        let target = tmp.path().join("a/b/file.bin");
        adapter
            .write_atomic(&abs(&target), LocalWriteStream(b"hello".to_vec()))
            .await
            .expect("write");

        // scan — target must appear in the results
        let scan = adapter.scan(&abs(tmp.path())).await.expect("scan");
        assert!(scan.0.iter().any(|(p, _)| p == &target));

        // hash
        let h = adapter.hash(&abs(&target)).await.expect("hash");
        let expected = blake3::hash(b"hello");
        assert_eq!(h.as_bytes(), expected.as_bytes());

        // read
        let r = adapter.read(&abs(&target)).await.expect("read");
        assert_eq!(r.0, b"hello");

        // rename
        let renamed = tmp.path().join("a/b/file.bin.renamed");
        adapter
            .rename(&abs(&target), &abs(&renamed))
            .await
            .expect("rename");
        assert!(!target.exists());
        assert!(renamed.exists());

        // delete
        adapter.delete(&abs(&renamed)).await.expect("delete");
        assert!(!renamed.exists());
    }
}
