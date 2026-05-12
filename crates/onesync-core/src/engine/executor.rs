//! Executor: drive one `FileOp` through the appropriate ports.
//!
//! Each `FileOp` runs through the relevant port call:
//! - `Upload`       → `LocalFs::read` → `RemoteDrive::upload_small` / `upload_session` (size-driven)
//! - `Download`     → `RemoteDrive::download` → `LocalFs::write_atomic`
//! - `LocalDelete`  → `LocalFs::delete`
//! - `RemoteDelete` → `RemoteDrive::delete`
//! - `LocalMkdir`   → `LocalFs::mkdir_p`
//! - `RemoteMkdir`  → `RemoteDrive::mkdir`
//! - `LocalRename`  → `LocalFs::rename` (target from `metadata["loser_target"]`)
//! - `RemoteRename` → `RemoteDrive::rename` (target from `metadata["loser_target"]`)
//!
//! See [`docs/spec/03-sync-engine.md`](../../../../docs/spec/03-sync-engine.md) §Op execution.

use onesync_protocol::{
    enums::{FileOpKind, FileOpStatus},
    file_op::FileOp,
    pair::Pair,
    path::AbsPath,
};

use crate::engine::types::EngineError;
use crate::limits::GRAPH_SMALL_UPLOAD_MAX_BYTES;
use crate::ports::{
    LocalFs, LocalWriteStream, RemoteDrive, StateStore,
    remote_drive::{RemoteItemId, UploadSession},
};

/// Dependencies needed by the executor.
pub struct ExecutorCtx<'a> {
    /// State store for status updates.
    pub store: &'a dyn StateStore,
    /// Local filesystem port.
    pub local: &'a dyn LocalFs,
    /// Remote drive port.
    pub remote: &'a dyn RemoteDrive,
}

/// Run a single `FileOp` end-to-end.
///
/// On success the op's status is updated to `Success` in the state store.
/// On failure the caller decides retry vs surface.
///
/// # Errors
///
/// Returns an [`EngineError`] if the operation fails.
pub async fn execute(ctx: &ExecutorCtx<'_>, op: &FileOp, pair: &Pair) -> Result<(), EngineError> {
    // Mark InProgress before any I/O.
    ctx.store
        .op_update_status(&op.id, FileOpStatus::InProgress)
        .await?;

    let result = match op.kind {
        FileOpKind::Upload => execute_upload(ctx, op, pair).await,
        FileOpKind::Download => execute_download(ctx, op, pair).await,
        FileOpKind::LocalDelete => execute_local_delete(ctx, op, pair).await,
        FileOpKind::RemoteDelete => execute_remote_delete(ctx, op).await,
        FileOpKind::LocalMkdir => execute_local_mkdir(ctx, op, pair).await,
        FileOpKind::RemoteMkdir => execute_remote_mkdir(ctx, op).await,
        FileOpKind::LocalRename => execute_local_rename(ctx, op, pair).await,
        FileOpKind::RemoteRename => execute_remote_rename(ctx, op).await,
    };

    match &result {
        Ok(()) => {
            ctx.store
                .op_update_status(&op.id, FileOpStatus::Success)
                .await?;
        }
        Err(_) => {
            ctx.store
                .op_update_status(&op.id, FileOpStatus::Backoff)
                .await?;
        }
    }

    result
}

/// Build the absolute local path for a given `op` by joining the pair's root + relative path.
fn local_abs(op: &FileOp, pair: &Pair) -> Result<AbsPath, EngineError> {
    let joined = format!("{}/{}", pair.local_path.as_str(), op.relative_path.as_str());
    joined.parse::<AbsPath>().map_err(|e| {
        EngineError::LocalFs(crate::ports::LocalFsError::InvalidPath {
            reason: e.to_string(),
        })
    })
}

/// Extract `metadata["loser_target"]` as a string.
fn loser_target_str(op: &FileOp) -> Result<&str, EngineError> {
    op.metadata
        .get("loser_target")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            EngineError::PairNotRunnable("rename op missing loser_target metadata".into())
        })
}

/// Upload: read local file, upload to remote (small or session).
async fn execute_upload(
    ctx: &ExecutorCtx<'_>,
    op: &FileOp,
    pair: &Pair,
) -> Result<(), EngineError> {
    let abs = local_abs(op, pair)?;
    let read_stream = ctx.local.read(&abs).await?;
    let bytes = &read_stream.0;
    let size = bytes.len() as u64;

    // Placeholder RemoteItemId — the production impl maps pair.remote_item_id here.
    // LINT: RemoteItemId is a unit-struct placeholder in the current port definition.
    if size <= GRAPH_SMALL_UPLOAD_MAX_BYTES {
        let _item = ctx
            .remote
            .upload_small(&RemoteItemId, op.relative_path.as_str(), bytes)
            .await?;
    } else {
        let _session = ctx
            .remote
            .upload_session(&RemoteItemId, op.relative_path.as_str(), size)
            .await?;
        // Drive the session — placeholder: the real impl streams chunks.
    }
    Ok(())
}

/// Download: fetch from remote, write atomically to local.
async fn execute_download(
    ctx: &ExecutorCtx<'_>,
    op: &FileOp,
    pair: &Pair,
) -> Result<(), EngineError> {
    let abs = local_abs(op, pair)?;
    // Placeholder RemoteItemId.
    let _remote_stream = ctx.remote.download(&RemoteItemId).await?;
    // RemoteReadStream is a placeholder; real impl reads bytes.
    let _side = ctx
        .local
        .write_atomic(&abs, LocalWriteStream(Vec::new()))
        .await?;
    Ok(())
}

/// Delete the local copy of a file.
async fn execute_local_delete(
    ctx: &ExecutorCtx<'_>,
    op: &FileOp,
    pair: &Pair,
) -> Result<(), EngineError> {
    let abs = local_abs(op, pair)?;
    ctx.local.delete(&abs).await?;
    Ok(())
}

/// Move the remote copy to the Recycle Bin.
async fn execute_remote_delete(ctx: &ExecutorCtx<'_>, op: &FileOp) -> Result<(), EngineError> {
    // Placeholder RemoteItemId.
    ctx.remote.delete(&RemoteItemId).await?;
    let _ = op; // op used for ID / metadata in production.
    Ok(())
}

/// Create the local directory (and any missing parents).
async fn execute_local_mkdir(
    ctx: &ExecutorCtx<'_>,
    op: &FileOp,
    pair: &Pair,
) -> Result<(), EngineError> {
    let abs = local_abs(op, pair)?;
    ctx.local.mkdir_p(&abs).await?;
    Ok(())
}

/// Create a remote directory under the pair root.
async fn execute_remote_mkdir(ctx: &ExecutorCtx<'_>, op: &FileOp) -> Result<(), EngineError> {
    // Placeholder RemoteItemId for the parent.
    let _item = ctx
        .remote
        .mkdir(&RemoteItemId, op.relative_path.as_str())
        .await?;
    Ok(())
}

/// Rename the local loser copy to its conflict target path.
async fn execute_local_rename(
    ctx: &ExecutorCtx<'_>,
    op: &FileOp,
    pair: &Pair,
) -> Result<(), EngineError> {
    let from = local_abs(op, pair)?;
    let loser = loser_target_str(op)?;
    let to_joined = format!("{}/{loser}", pair.local_path.as_str());
    let to = to_joined.parse::<AbsPath>().map_err(|e| {
        EngineError::LocalFs(crate::ports::LocalFsError::InvalidPath {
            reason: e.to_string(),
        })
    })?;
    ctx.local.rename(&from, &to).await?;
    Ok(())
}

/// Rename the remote loser copy to its conflict target name.
async fn execute_remote_rename(ctx: &ExecutorCtx<'_>, op: &FileOp) -> Result<(), EngineError> {
    let loser = loser_target_str(op)?;
    // Placeholder RemoteItemId for the remote item to rename.
    let _item = ctx.remote.rename(&RemoteItemId, loser).await?;
    Ok(())
}

// Suppress unused-import lint for placeholder types used in future expansions.
#[allow(dead_code, clippy::missing_const_for_fn)]
fn _keep_upload_session_in_scope() -> Option<UploadSession> {
    None
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    clippy::significant_drop_in_scrutinee
)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use onesync_protocol::{
        account::Account,
        audit::AuditEvent,
        conflict::Conflict,
        enums::{FileKind, FileOpKind, FileOpStatus, PairStatus},
        file_entry::FileEntry,
        file_op::FileOp,
        file_side::FileSide,
        id::{AccountTag, Id, PairId, PairTag, SyncRunTag},
        pair::Pair,
        path::{AbsPath, RelPath},
        primitives::{ContentHash, DriveItemId, Timestamp},
        sync_run::SyncRun,
    };
    use std::{collections::HashMap, sync::Mutex};
    use ulid::Ulid;

    use crate::ports::{
        GraphError, LocalEventStream, LocalFsError, LocalReadStream, LocalScanStream,
        LocalWriteStream, StateError,
        remote_drive::{
            AccessToken, AccountProfile, DeltaPage, RemoteItem, RemoteItemId, RemoteReadStream,
            UploadSession,
        },
    };

    // ─── minimal in-test StateStore ───────────────────────────────────────────

    #[derive(Default)]
    struct TestStore {
        statuses: Mutex<HashMap<String, FileOpStatus>>,
    }

    #[async_trait]
    impl crate::ports::StateStore for TestStore {
        async fn account_upsert(&self, _: &Account) -> Result<(), StateError> {
            Ok(())
        }
        async fn account_get(
            &self,
            _: &onesync_protocol::id::AccountId,
        ) -> Result<Option<Account>, StateError> {
            Ok(None)
        }
        async fn pair_upsert(&self, _: &Pair) -> Result<(), StateError> {
            Ok(())
        }
        async fn pair_get(&self, _: &PairId) -> Result<Option<Pair>, StateError> {
            Ok(None)
        }
        async fn pairs_active(&self) -> Result<Vec<Pair>, StateError> {
            Ok(Vec::new())
        }
        async fn file_entry_upsert(&self, _: &FileEntry) -> Result<(), StateError> {
            Ok(())
        }
        async fn file_entry_get(
            &self,
            _: &PairId,
            _: &RelPath,
        ) -> Result<Option<FileEntry>, StateError> {
            Ok(None)
        }
        async fn file_entries_dirty(
            &self,
            _: &PairId,
            _: usize,
        ) -> Result<Vec<FileEntry>, StateError> {
            Ok(Vec::new())
        }
        async fn run_record(&self, _: &SyncRun) -> Result<(), StateError> {
            Ok(())
        }
        async fn op_insert(&self, op: &FileOp) -> Result<(), StateError> {
            self.statuses
                .lock()
                .unwrap()
                .insert(op.id.to_string(), op.status);
            Ok(())
        }
        async fn op_update_status(
            &self,
            id: &onesync_protocol::id::FileOpId,
            status: FileOpStatus,
        ) -> Result<(), StateError> {
            self.statuses.lock().unwrap().insert(id.to_string(), status);
            Ok(())
        }
        async fn conflict_insert(&self, _: &Conflict) -> Result<(), StateError> {
            Ok(())
        }
        async fn conflicts_unresolved(&self, _: &PairId) -> Result<Vec<Conflict>, StateError> {
            Ok(Vec::new())
        }
        async fn audit_append(&self, _: &AuditEvent) -> Result<(), StateError> {
            Ok(())
        }
    }

    // ─── minimal in-test LocalFs ──────────────────────────────────────────────

    #[derive(Default)]
    struct TestLocalFs {
        files: Mutex<HashMap<String, Vec<u8>>>,
    }

    #[async_trait]
    impl crate::ports::LocalFs for TestLocalFs {
        async fn scan(&self, _: &AbsPath) -> Result<LocalScanStream, LocalFsError> {
            Ok(LocalScanStream(Vec::new()))
        }
        async fn read(&self, path: &AbsPath) -> Result<LocalReadStream, LocalFsError> {
            let guard = self.files.lock().unwrap();
            guard
                .get(path.as_str())
                .cloned()
                .map(LocalReadStream)
                .ok_or_else(|| LocalFsError::NotFound(path.as_str().to_owned()))
        }
        async fn write_atomic(
            &self,
            path: &AbsPath,
            stream: LocalWriteStream,
        ) -> Result<FileSide, LocalFsError> {
            self.files
                .lock()
                .unwrap()
                .insert(path.as_str().to_owned(), stream.0.clone());
            Ok(FileSide {
                kind: FileKind::File,
                size_bytes: stream.0.len() as u64,
                content_hash: Some(ContentHash::from_bytes(*blake3::hash(&stream.0).as_bytes())),
                // LINT: test-double surface; using a fixed epoch time avoids Clock injection.
                #[allow(clippy::disallowed_methods)]
                mtime: Timestamp::from_datetime(chrono::Utc::now()),
                etag: None,
                remote_item_id: None,
            })
        }
        async fn rename(&self, from: &AbsPath, to: &AbsPath) -> Result<(), LocalFsError> {
            // LINT: the guard must live across both operations; early-drop would be incorrect.
            #[allow(clippy::significant_drop_tightening)]
            let data = {
                let mut guard = self.files.lock().unwrap();
                let data = guard
                    .remove(from.as_str())
                    .ok_or_else(|| LocalFsError::NotFound(from.as_str().to_owned()))?;
                guard.insert(to.as_str().to_owned(), data.clone());
                data
            };
            let _ = data;
            Ok(())
        }
        async fn delete(&self, path: &AbsPath) -> Result<(), LocalFsError> {
            self.files
                .lock()
                .unwrap()
                .remove(path.as_str())
                .ok_or_else(|| LocalFsError::NotFound(path.as_str().to_owned()))
                .map(|_| ())
        }
        async fn mkdir_p(&self, path: &AbsPath) -> Result<(), LocalFsError> {
            self.files
                .lock()
                .unwrap()
                .insert(path.as_str().to_owned(), Vec::new());
            Ok(())
        }
        async fn watch(&self, _: &AbsPath) -> Result<LocalEventStream, LocalFsError> {
            let (_, rx) = tokio::sync::mpsc::channel(1);
            Ok(LocalEventStream(rx))
        }
        async fn hash(&self, path: &AbsPath) -> Result<ContentHash, LocalFsError> {
            let guard = self.files.lock().unwrap();
            guard
                .get(path.as_str())
                .map(|b| ContentHash::from_bytes(*blake3::hash(b).as_bytes()))
                .ok_or_else(|| LocalFsError::NotFound(path.as_str().to_owned()))
        }
    }

    // ─── minimal in-test RemoteDrive ──────────────────────────────────────────

    #[derive(Default)]
    struct TestRemoteDrive {
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl crate::ports::RemoteDrive for TestRemoteDrive {
        async fn account_profile(&self, _: &AccessToken) -> Result<AccountProfile, GraphError> {
            Ok(AccountProfile)
        }
        async fn item_by_path(
            &self,
            _: &onesync_protocol::primitives::DriveId,
            _: &str,
        ) -> Result<Option<RemoteItem>, GraphError> {
            Ok(None)
        }
        async fn delta(
            &self,
            _: &onesync_protocol::primitives::DriveId,
            _: Option<&onesync_protocol::primitives::DeltaCursor>,
        ) -> Result<DeltaPage, GraphError> {
            Ok(DeltaPage)
        }
        async fn download(&self, _: &RemoteItemId) -> Result<RemoteReadStream, GraphError> {
            self.calls.lock().unwrap().push("download".into());
            Ok(RemoteReadStream)
        }
        async fn upload_small(
            &self,
            _: &RemoteItemId,
            _: &str,
            _: &[u8],
        ) -> Result<RemoteItem, GraphError> {
            self.calls.lock().unwrap().push("upload_small".into());
            Ok(RemoteItem)
        }
        async fn upload_session(
            &self,
            _: &RemoteItemId,
            _: &str,
            _: u64,
        ) -> Result<UploadSession, GraphError> {
            self.calls.lock().unwrap().push("upload_session".into());
            Ok(UploadSession)
        }
        async fn rename(&self, _: &RemoteItemId, name: &str) -> Result<RemoteItem, GraphError> {
            self.calls.lock().unwrap().push(format!("rename:{name}"));
            Ok(RemoteItem)
        }
        async fn delete(&self, _: &RemoteItemId) -> Result<(), GraphError> {
            self.calls.lock().unwrap().push("delete".into());
            Ok(())
        }
        async fn mkdir(&self, _: &RemoteItemId, name: &str) -> Result<RemoteItem, GraphError> {
            self.calls.lock().unwrap().push(format!("mkdir:{name}"));
            Ok(RemoteItem)
        }
    }

    // ─── helpers ──────────────────────────────────────────────────────────────

    fn ts() -> Timestamp {
        Timestamp::from_datetime(
            chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 5, 12, 0, 0, 0).unwrap(),
        )
    }

    fn make_pair() -> Pair {
        Pair {
            id: Id::<PairTag>::from_ulid(Ulid::from(1u128)),
            account_id: Id::<AccountTag>::from_ulid(Ulid::from(2u128)),
            local_path: "/tmp/sync".parse::<AbsPath>().unwrap(),
            remote_item_id: DriveItemId::new("root"),
            remote_path: "/".into(),
            display_name: "Test".into(),
            status: PairStatus::Active,
            paused: false,
            delta_token: None,
            errored_reason: None,
            created_at: ts(),
            updated_at: ts(),
            last_sync_at: None,
            conflict_count: 0,
        }
    }

    fn make_op(
        kind: FileOpKind,
        path: &str,
        meta: serde_json::Map<String, serde_json::Value>,
    ) -> FileOp {
        let pair_id = Id::<PairTag>::from_ulid(Ulid::from(1u128));
        let run_id = Id::<SyncRunTag>::from_ulid(Ulid::from(3u128));
        FileOp {
            id: Id::from_ulid(Ulid::from(99u128)),
            run_id,
            pair_id,
            relative_path: path.parse::<RelPath>().unwrap(),
            kind,
            status: FileOpStatus::Enqueued,
            attempts: 0,
            last_error: None,
            metadata: meta,
            enqueued_at: ts(),
            started_at: None,
            finished_at: None,
        }
    }

    // ─── tests ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn upload_small_calls_remote() {
        let store = TestStore::default();
        let local = TestLocalFs::default();
        let remote = TestRemoteDrive::default();
        let pair = make_pair();

        // Seed the local file.
        local
            .files
            .lock()
            .unwrap()
            .insert("/tmp/sync/file.txt".into(), b"hello".to_vec());

        let op = make_op(FileOpKind::Upload, "file.txt", serde_json::Map::default());
        let ctx = ExecutorCtx {
            store: &store,
            local: &local,
            remote: &remote,
        };
        execute(&ctx, &op, &pair).await.unwrap();
        assert!(
            remote
                .calls
                .lock()
                .unwrap()
                .contains(&"upload_small".into())
        );
        let statuses = store.statuses.lock().unwrap();
        assert_eq!(
            *statuses.get(&op.id.to_string()).unwrap(),
            FileOpStatus::Success
        );
    }

    #[tokio::test]
    async fn download_calls_local_write() {
        let store = TestStore::default();
        let local = TestLocalFs::default();
        let remote = TestRemoteDrive::default();
        let pair = make_pair();

        let op = make_op(FileOpKind::Download, "file.txt", serde_json::Map::default());
        let ctx = ExecutorCtx {
            store: &store,
            local: &local,
            remote: &remote,
        };
        execute(&ctx, &op, &pair).await.unwrap();
        assert!(remote.calls.lock().unwrap().contains(&"download".into()));
    }

    #[tokio::test]
    async fn local_delete_removes_file() {
        let store = TestStore::default();
        let local = TestLocalFs::default();
        let remote = TestRemoteDrive::default();
        let pair = make_pair();

        local
            .files
            .lock()
            .unwrap()
            .insert("/tmp/sync/gone.txt".into(), b"bye".to_vec());

        let op = make_op(
            FileOpKind::LocalDelete,
            "gone.txt",
            serde_json::Map::default(),
        );
        let ctx = ExecutorCtx {
            store: &store,
            local: &local,
            remote: &remote,
        };
        execute(&ctx, &op, &pair).await.unwrap();
        assert!(
            !local
                .files
                .lock()
                .unwrap()
                .contains_key("/tmp/sync/gone.txt")
        );
    }

    #[tokio::test]
    async fn remote_delete_calls_remote() {
        let store = TestStore::default();
        let local = TestLocalFs::default();
        let remote = TestRemoteDrive::default();
        let pair = make_pair();

        let op = make_op(
            FileOpKind::RemoteDelete,
            "gone.txt",
            serde_json::Map::default(),
        );
        let ctx = ExecutorCtx {
            store: &store,
            local: &local,
            remote: &remote,
        };
        execute(&ctx, &op, &pair).await.unwrap();
        assert!(remote.calls.lock().unwrap().contains(&"delete".into()));
    }

    #[tokio::test]
    async fn local_mkdir_creates_dir() {
        let store = TestStore::default();
        let local = TestLocalFs::default();
        let remote = TestRemoteDrive::default();
        let pair = make_pair();

        let op = make_op(FileOpKind::LocalMkdir, "docs", serde_json::Map::default());
        let ctx = ExecutorCtx {
            store: &store,
            local: &local,
            remote: &remote,
        };
        execute(&ctx, &op, &pair).await.unwrap();
        assert!(local.files.lock().unwrap().contains_key("/tmp/sync/docs"));
    }

    #[tokio::test]
    async fn remote_mkdir_calls_remote() {
        let store = TestStore::default();
        let local = TestLocalFs::default();
        let remote = TestRemoteDrive::default();
        let pair = make_pair();

        let op = make_op(FileOpKind::RemoteMkdir, "docs", serde_json::Map::default());
        let ctx = ExecutorCtx {
            store: &store,
            local: &local,
            remote: &remote,
        };
        execute(&ctx, &op, &pair).await.unwrap();
        let calls = remote.calls.lock().unwrap();
        assert!(calls.iter().any(|c| c.starts_with("mkdir:")));
    }

    #[tokio::test]
    async fn local_rename_moves_file() {
        let store = TestStore::default();
        let local = TestLocalFs::default();
        let remote = TestRemoteDrive::default();
        let pair = make_pair();

        local
            .files
            .lock()
            .unwrap()
            .insert("/tmp/sync/original.txt".into(), b"data".to_vec());

        let mut meta = serde_json::Map::default();
        meta.insert(
            "loser_target".into(),
            serde_json::Value::String(
                "original (conflict 2026-01-01T00-00-00Z from host).txt".into(),
            ),
        );
        let op = make_op(FileOpKind::LocalRename, "original.txt", meta);
        let ctx = ExecutorCtx {
            store: &store,
            local: &local,
            remote: &remote,
        };
        execute(&ctx, &op, &pair).await.unwrap();
        let files = local.files.lock().unwrap();
        assert!(!files.contains_key("/tmp/sync/original.txt"));
        let key = "/tmp/sync/original (conflict 2026-01-01T00-00-00Z from host).txt";
        assert!(files.contains_key(key));
    }

    #[tokio::test]
    async fn remote_rename_calls_remote() {
        let store = TestStore::default();
        let local = TestLocalFs::default();
        let remote = TestRemoteDrive::default();
        let pair = make_pair();

        let mut meta = serde_json::Map::default();
        meta.insert(
            "loser_target".into(),
            serde_json::Value::String("file (conflict 2026-01-01T00-00-00Z from host).txt".into()),
        );
        let op = make_op(FileOpKind::RemoteRename, "file.txt", meta);
        let ctx = ExecutorCtx {
            store: &store,
            local: &local,
            remote: &remote,
        };
        execute(&ctx, &op, &pair).await.unwrap();
        let calls = remote.calls.lock().unwrap();
        assert!(calls.iter().any(|c| c.starts_with("rename:")));
    }
}
