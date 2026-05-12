//! `StateStore` port: durable storage for accounts, pairs, file entries, ops, conflicts, runs, audit events.

use async_trait::async_trait;
use onesync_protocol::{
    account::Account,
    audit::AuditEvent,
    conflict::Conflict,
    file_entry::FileEntry,
    file_op::FileOp,
    id::{AccountId, FileOpId, PairId},
    pair::Pair,
    path::RelPath,
    sync_run::SyncRun,
};

/// Errors returned by `StateStore` operations.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// Underlying I/O or backend failure.
    #[error("backend i/o: {0}")]
    Io(String),
    /// Schema version mismatch detected at startup or migration time.
    #[error("schema mismatch: {0}")]
    Schema(String),
    /// Entity not found.
    #[error("not found: {0}")]
    NotFound(String),
    /// A storage constraint was violated (uniqueness, foreign key, etc.).
    #[error("constraint violated: {0}")]
    Constraint(String),
}

/// Durable storage for the engine.
#[async_trait]
pub trait StateStore: Send + Sync {
    /// Insert or update an account.
    async fn account_upsert(&self, account: &Account) -> Result<(), StateError>;
    /// Fetch one account by id, or `None` if not found.
    async fn account_get(&self, id: &AccountId) -> Result<Option<Account>, StateError>;
    /// Insert or update a pair.
    async fn pair_upsert(&self, pair: &Pair) -> Result<(), StateError>;
    /// Fetch one pair by id, or `None` if not found.
    async fn pair_get(&self, id: &PairId) -> Result<Option<Pair>, StateError>;
    /// Return all pairs with status other than `Removed`.
    async fn pairs_active(&self) -> Result<Vec<Pair>, StateError>;
    /// Insert or update a file entry keyed by `(pair_id, relative_path)`.
    async fn file_entry_upsert(&self, entry: &FileEntry) -> Result<(), StateError>;
    /// Fetch one file entry by composite key, or `None`.
    async fn file_entry_get(
        &self,
        pair: &PairId,
        path: &RelPath,
    ) -> Result<Option<FileEntry>, StateError>;
    /// Return up to `limit` dirty entries for a pair, ordered by `updated_at` ascending.
    async fn file_entries_dirty(
        &self,
        pair: &PairId,
        limit: usize,
    ) -> Result<Vec<FileEntry>, StateError>;
    /// Persist a finished sync-run record.
    async fn run_record(&self, run: &SyncRun) -> Result<(), StateError>;
    /// Insert a new file operation.
    async fn op_insert(&self, op: &FileOp) -> Result<(), StateError>;
    /// Update a file op's status.
    async fn op_update_status(
        &self,
        id: &FileOpId,
        status: onesync_protocol::enums::FileOpStatus,
    ) -> Result<(), StateError>;
    /// Insert a new conflict record.
    async fn conflict_insert(&self, c: &Conflict) -> Result<(), StateError>;
    /// Return all unresolved conflicts for a pair.
    async fn conflicts_unresolved(&self, pair: &PairId) -> Result<Vec<Conflict>, StateError>;
    /// Append a structured audit event.
    async fn audit_append(&self, evt: &AuditEvent) -> Result<(), StateError>;
}
