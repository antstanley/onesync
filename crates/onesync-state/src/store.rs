//! `SqliteStore` — concrete `StateStore` adapter.

use async_trait::async_trait;

use onesync_core::ports::{StateError, StateStore};
use onesync_protocol::{
    account::Account,
    audit::AuditEvent,
    conflict::Conflict,
    enums::FileOpStatus,
    file_entry::FileEntry,
    file_op::FileOp,
    id::{AccountId, FileOpId, PairId},
    pair::Pair,
    path::RelPath,
    sync_run::SyncRun,
};

use crate::connection::ConnectionPool;
use crate::error::StateStoreError;
use crate::queries;

/// SQLite-backed `StateStore` adapter.
#[derive(Clone, Debug)]
pub struct SqliteStore {
    pool: ConnectionPool,
}

impl SqliteStore {
    /// Construct a `SqliteStore` backed by the given pool.
    #[must_use]
    pub const fn new(pool: ConnectionPool) -> Self {
        Self { pool }
    }
}

fn map_err(e: StateStoreError) -> StateError {
    match e {
        StateStoreError::Sqlite(s) => StateError::Io(s),
        StateStoreError::Migration(s) | StateStoreError::Schema(s) => StateError::Schema(s),
    }
}

#[async_trait]
impl StateStore for SqliteStore {
    async fn account_upsert(&self, account: &Account) -> Result<(), StateError> {
        let pool = self.pool.clone();
        let account = account.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::accounts::upsert(&conn, &account).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn account_get(&self, id: &AccountId) -> Result<Option<Account>, StateError> {
        let pool = self.pool.clone();
        let id = *id;
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::accounts::get(&conn, &id).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn pair_upsert(&self, pair: &Pair) -> Result<(), StateError> {
        let pool = self.pool.clone();
        let pair = pair.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::pairs::upsert(&conn, &pair).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn pair_get(&self, id: &PairId) -> Result<Option<Pair>, StateError> {
        let pool = self.pool.clone();
        let id = *id;
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::pairs::get(&conn, &id).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn pairs_active(&self) -> Result<Vec<Pair>, StateError> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::pairs::active(&conn).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn file_entry_upsert(&self, entry: &FileEntry) -> Result<(), StateError> {
        let pool = self.pool.clone();
        let entry = entry.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::file_entries::upsert(&conn, &entry).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn file_entry_get(
        &self,
        pair: &PairId,
        path: &RelPath,
    ) -> Result<Option<FileEntry>, StateError> {
        let pool = self.pool.clone();
        let pair = *pair;
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::file_entries::get(&conn, &pair, &path).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn file_entries_dirty(
        &self,
        pair: &PairId,
        limit: usize,
    ) -> Result<Vec<FileEntry>, StateError> {
        let pool = self.pool.clone();
        let pair = *pair;
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::file_entries::dirty(&conn, &pair, limit).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn run_record(&self, run: &SyncRun) -> Result<(), StateError> {
        let pool = self.pool.clone();
        let run = run.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::sync_runs::record(&conn, &run).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn op_insert(&self, op: &FileOp) -> Result<(), StateError> {
        let pool = self.pool.clone();
        let op = op.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::file_ops::insert(&conn, &op).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn op_update_status(
        &self,
        id: &FileOpId,
        status: FileOpStatus,
    ) -> Result<(), StateError> {
        let pool = self.pool.clone();
        let id = *id;
        let now = onesync_protocol::primitives::Timestamp::from_datetime({
            #[allow(clippy::disallowed_methods)]
            // LINT: timestamp source. Refactor to inject a Clock when the engine wires this up.
            chrono::Utc::now()
        });
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::file_ops::update_status(&conn, &id, status, &now).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn conflict_insert(&self, c: &Conflict) -> Result<(), StateError> {
        let pool = self.pool.clone();
        let c = c.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::conflicts::insert(&conn, &c).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn conflicts_unresolved(&self, pair: &PairId) -> Result<Vec<Conflict>, StateError> {
        let pool = self.pool.clone();
        let pair = *pair;
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::conflicts::unresolved(&conn, &pair).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }

    async fn audit_append(&self, evt: &AuditEvent) -> Result<(), StateError> {
        let pool = self.pool.clone();
        let evt = evt.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(map_err)?;
            queries::audit::append(&conn, &evt).map_err(map_err)
        })
        .await
        .map_err(|e| StateError::Io(format!("join: {e}")))?
    }
}
