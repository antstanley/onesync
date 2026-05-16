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
    /// Fetch one file entry by `(pair_id, case-folded relative_path)`, or
    /// `None`. RP1-F24: APFS folds `Foo.txt` and `foo.txt` to the same
    /// inode but the engine stores them under their original byte form; an
    /// exact-match lookup will miss when a remote rename arrives at a
    /// different case from what local stored. Callers that need to detect
    /// such drift use this method.
    ///
    /// The fold is ASCII-only here, matching
    /// `crate::engine::case_collision::case_folds_equal`. Full Unicode
    /// folding is RP1-F15 territory.
    async fn file_entry_get_ci(
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

    // ── M8 additions (RPC handler wiring) ────────────────────────────────

    /// Return the singleton instance config; `None` if uninitialised.
    async fn config_get(
        &self,
    ) -> Result<Option<onesync_protocol::config::InstanceConfig>, StateError>;
    /// Insert or update the singleton instance config.
    async fn config_upsert(
        &self,
        cfg: &onesync_protocol::config::InstanceConfig,
    ) -> Result<(), StateError>;
    /// List accounts (no filter; small N).
    async fn accounts_list(&self) -> Result<Vec<Account>, StateError>;
    /// Delete an account row (FK cascades remove pairs).
    async fn account_remove(&self, id: &AccountId) -> Result<(), StateError>;
    /// List pairs (optionally filtered by account); `include_removed` controls
    /// whether soft-deleted rows are included.
    async fn pairs_list(
        &self,
        account: Option<&AccountId>,
        include_removed: bool,
    ) -> Result<Vec<Pair>, StateError>;
    /// Fetch one conflict by id.
    async fn conflict_get(
        &self,
        id: &onesync_protocol::id::ConflictId,
    ) -> Result<Option<Conflict>, StateError>;
    /// Mark a conflict resolved.
    async fn conflict_resolve(
        &self,
        id: &onesync_protocol::id::ConflictId,
        resolution: onesync_protocol::enums::ConflictResolution,
        resolved_at: onesync_protocol::primitives::Timestamp,
        note: Option<String>,
    ) -> Result<(), StateError>;
    /// Recent sync runs for a pair (newest first).
    async fn runs_recent(&self, pair: &PairId, limit: usize) -> Result<Vec<SyncRun>, StateError>;
    /// Fetch one sync run by id.
    async fn run_get(
        &self,
        id: &onesync_protocol::id::SyncRunId,
    ) -> Result<Option<SyncRun>, StateError>;
    /// Recent audit events (newest first).
    async fn audit_recent(&self, limit: usize) -> Result<Vec<AuditEvent>, StateError>;
    /// Audit-event time-window search.
    async fn audit_search(
        &self,
        from_ts: &onesync_protocol::primitives::Timestamp,
        to_ts: &onesync_protocol::primitives::Timestamp,
        level: Option<onesync_protocol::enums::AuditLevel>,
        pair: Option<&PairId>,
        limit: usize,
    ) -> Result<Vec<AuditEvent>, StateError>;

    /// Write a consistent snapshot of the database to `to`. Implementations should use
    /// `VACUUM INTO` so the WAL is fully captured.
    async fn backup_to(&self, to: &std::path::Path) -> Result<(), StateError>;

    /// Run retention pruning + `VACUUM` to reclaim space. Wired by `state.compact.now`.
    async fn compact_now(
        &self,
        now: &onesync_protocol::primitives::Timestamp,
    ) -> Result<(), StateError>;
}
