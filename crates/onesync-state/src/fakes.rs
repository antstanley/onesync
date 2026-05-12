//! In-memory `StateStore` for engine tests.

#![cfg(any(test, feature = "fakes"))]
#![allow(clippy::expect_used)]
// LINT: this module is the test-double surface for the StateStore port;
//       mutex-poison expects are the standard pattern.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use onesync_core::ports::{StateError, StateStore};
use onesync_protocol::{
    account::Account,
    audit::AuditEvent,
    config::InstanceConfig,
    conflict::Conflict,
    enums::{AuditLevel, ConflictResolution, FileOpStatus, FileSyncState, PairStatus},
    file_entry::FileEntry,
    file_op::FileOp,
    id::{AccountId, ConflictId, FileOpId, PairId, SyncRunId},
    pair::Pair,
    path::RelPath,
    primitives::Timestamp,
    sync_run::SyncRun,
};

/// In-memory `StateStore` for use in engine tests.
#[derive(Default, Debug)]
pub struct InMemoryStore {
    accounts: Mutex<HashMap<AccountId, Account>>,
    pairs: Mutex<HashMap<PairId, Pair>>,
    file_entries: Mutex<HashMap<(PairId, RelPath), FileEntry>>,
    file_ops: Mutex<HashMap<FileOpId, FileOp>>,
    conflicts: Mutex<HashMap<ConflictId, Conflict>>,
    sync_runs: Mutex<HashMap<SyncRunId, SyncRun>>,
    audit: Mutex<Vec<AuditEvent>>,
    config: Mutex<Option<InstanceConfig>>,
}

impl InMemoryStore {
    /// New empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl StateStore for InMemoryStore {
    async fn account_upsert(&self, account: &Account) -> Result<(), StateError> {
        self.accounts
            .lock()
            .expect("acct lock")
            .insert(account.id, account.clone());
        Ok(())
    }

    async fn account_get(&self, id: &AccountId) -> Result<Option<Account>, StateError> {
        Ok(self.accounts.lock().expect("acct lock").get(id).cloned())
    }

    async fn pair_upsert(&self, pair: &Pair) -> Result<(), StateError> {
        self.pairs
            .lock()
            .expect("pair lock")
            .insert(pair.id, pair.clone());
        Ok(())
    }

    async fn pair_get(&self, id: &PairId) -> Result<Option<Pair>, StateError> {
        Ok(self.pairs.lock().expect("pair lock").get(id).cloned())
    }

    async fn pairs_active(&self) -> Result<Vec<Pair>, StateError> {
        let mut out: Vec<Pair> = self
            .pairs
            .lock()
            .expect("pair lock")
            .values()
            .filter(|p| p.status != PairStatus::Removed)
            .cloned()
            .collect();
        out.sort_by_key(|a| a.id.to_string());
        Ok(out)
    }

    async fn file_entry_upsert(&self, entry: &FileEntry) -> Result<(), StateError> {
        let key = (entry.pair_id, entry.relative_path.clone());
        self.file_entries
            .lock()
            .expect("fe lock")
            .insert(key, entry.clone());
        Ok(())
    }

    async fn file_entry_get(
        &self,
        pair: &PairId,
        path: &RelPath,
    ) -> Result<Option<FileEntry>, StateError> {
        let key = (*pair, path.clone());
        Ok(self
            .file_entries
            .lock()
            .expect("fe lock")
            .get(&key)
            .cloned())
    }

    async fn file_entries_dirty(
        &self,
        pair: &PairId,
        limit: usize,
    ) -> Result<Vec<FileEntry>, StateError> {
        let mut matching: Vec<FileEntry> = self
            .file_entries
            .lock()
            .expect("fe lock")
            .values()
            .filter(|e| &e.pair_id == pair && e.sync_state != FileSyncState::Clean)
            .cloned()
            .collect();
        matching.sort_by_key(|a| a.updated_at);
        matching.truncate(limit);
        Ok(matching)
    }

    async fn run_record(&self, run: &SyncRun) -> Result<(), StateError> {
        self.sync_runs
            .lock()
            .expect("run lock")
            .insert(run.id, run.clone());
        Ok(())
    }

    async fn op_insert(&self, op: &FileOp) -> Result<(), StateError> {
        self.file_ops
            .lock()
            .expect("op lock")
            .insert(op.id, op.clone());
        Ok(())
    }

    async fn op_update_status(
        &self,
        id: &FileOpId,
        status: FileOpStatus,
    ) -> Result<(), StateError> {
        // Mirror SqliteStore's UPDATE behaviour: silently no-op when the row does not exist
        // (SQLite UPDATE affecting 0 rows returns Ok(0) which is mapped to Ok(())).
        // Known divergence: the fake could return NotFound here instead, but matching
        // SqliteStore's silent-success is more faithful for parity testing.
        if let Some(op) = self.file_ops.lock().expect("op lock").get_mut(id) {
            op.status = status;
        }
        Ok(())
    }

    async fn conflict_insert(&self, c: &Conflict) -> Result<(), StateError> {
        self.conflicts
            .lock()
            .expect("cf lock")
            .insert(c.id, c.clone());
        Ok(())
    }

    async fn conflicts_unresolved(&self, pair: &PairId) -> Result<Vec<Conflict>, StateError> {
        let mut out: Vec<Conflict> = self
            .conflicts
            .lock()
            .expect("cf lock")
            .values()
            .filter(|c| &c.pair_id == pair && c.resolved_at.is_none())
            .cloned()
            .collect();
        out.sort_by_key(|a| a.detected_at);
        Ok(out)
    }

    async fn audit_append(&self, evt: &AuditEvent) -> Result<(), StateError> {
        self.audit.lock().expect("audit lock").push(evt.clone());
        Ok(())
    }

    async fn config_get(&self) -> Result<Option<InstanceConfig>, StateError> {
        Ok(self.config.lock().expect("cfg lock").clone())
    }

    async fn config_upsert(&self, cfg: &InstanceConfig) -> Result<(), StateError> {
        *self.config.lock().expect("cfg lock") = Some(cfg.clone());
        Ok(())
    }

    async fn accounts_list(&self) -> Result<Vec<Account>, StateError> {
        let mut out: Vec<Account> = self
            .accounts
            .lock()
            .expect("acct lock")
            .values()
            .cloned()
            .collect();
        out.sort_by_key(|a| a.id.to_string());
        Ok(out)
    }

    async fn account_remove(&self, id: &AccountId) -> Result<(), StateError> {
        // Cascade: remove pairs and dependent rows for this account, matching SQLite FK behaviour.
        let mut accts = self.accounts.lock().expect("acct lock");
        accts.remove(id);
        drop(accts);

        let pair_ids: Vec<PairId> = {
            let pairs = self.pairs.lock().expect("pair lock");
            pairs
                .values()
                .filter(|p| &p.account_id == id)
                .map(|p| p.id)
                .collect()
        };
        let mut pairs = self.pairs.lock().expect("pair lock");
        for pid in &pair_ids {
            pairs.remove(pid);
        }
        drop(pairs);

        let pair_set: std::collections::HashSet<PairId> = pair_ids.into_iter().collect();
        self.file_entries
            .lock()
            .expect("fe lock")
            .retain(|(p, _), _| !pair_set.contains(p));
        self.file_ops
            .lock()
            .expect("op lock")
            .retain(|_, op| !pair_set.contains(&op.pair_id));
        self.conflicts
            .lock()
            .expect("cf lock")
            .retain(|_, c| !pair_set.contains(&c.pair_id));
        self.sync_runs
            .lock()
            .expect("run lock")
            .retain(|_, r| !pair_set.contains(&r.pair_id));
        Ok(())
    }

    async fn pairs_list(
        &self,
        account: Option<&AccountId>,
        include_removed: bool,
    ) -> Result<Vec<Pair>, StateError> {
        let mut out: Vec<Pair> = self
            .pairs
            .lock()
            .expect("pair lock")
            .values()
            .filter(|p| account.is_none_or(|a| &p.account_id == a))
            .filter(|p| include_removed || p.status != PairStatus::Removed)
            .cloned()
            .collect();
        out.sort_by_key(|p| p.id.to_string());
        Ok(out)
    }

    async fn conflict_get(&self, id: &ConflictId) -> Result<Option<Conflict>, StateError> {
        Ok(self.conflicts.lock().expect("cf lock").get(id).cloned())
    }

    async fn conflict_resolve(
        &self,
        id: &ConflictId,
        resolution: ConflictResolution,
        resolved_at: Timestamp,
        note: Option<String>,
    ) -> Result<(), StateError> {
        if let Some(c) = self.conflicts.lock().expect("cf lock").get_mut(id) {
            c.resolved_at = Some(resolved_at);
            c.resolution = Some(resolution);
            if note.is_some() {
                c.note = note;
            }
        }
        Ok(())
    }

    async fn runs_recent(&self, pair: &PairId, limit: usize) -> Result<Vec<SyncRun>, StateError> {
        let mut out: Vec<SyncRun> = self
            .sync_runs
            .lock()
            .expect("run lock")
            .values()
            .filter(|r| &r.pair_id == pair)
            .cloned()
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.started_at));
        out.truncate(limit);
        Ok(out)
    }

    async fn run_get(&self, id: &SyncRunId) -> Result<Option<SyncRun>, StateError> {
        Ok(self.sync_runs.lock().expect("run lock").get(id).cloned())
    }

    async fn audit_recent(&self, limit: usize) -> Result<Vec<AuditEvent>, StateError> {
        let mut out: Vec<AuditEvent> = self.audit.lock().expect("audit lock").clone();
        out.sort_by_key(|e| std::cmp::Reverse(e.ts));
        out.truncate(limit);
        Ok(out)
    }

    async fn audit_search(
        &self,
        from_ts: &Timestamp,
        to_ts: &Timestamp,
        level: Option<AuditLevel>,
        pair: Option<&PairId>,
        limit: usize,
    ) -> Result<Vec<AuditEvent>, StateError> {
        let mut out: Vec<AuditEvent> = self
            .audit
            .lock()
            .expect("audit lock")
            .iter()
            .filter(|e| &e.ts >= from_ts && &e.ts <= to_ts)
            .filter(|e| level.is_none_or(|l| e.level == l))
            .filter(|e| pair.is_none_or(|p| e.pair_id.as_ref() == Some(p)))
            .cloned()
            .collect();
        out.sort_by_key(|e| std::cmp::Reverse(e.ts));
        out.truncate(limit);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use crate::{SqliteStore, open};
    use chrono::{TimeZone, Utc};
    use onesync_core::ports::StateStore;
    use onesync_protocol::{
        account::Account,
        audit::AuditEvent,
        conflict::Conflict,
        enums::{
            AccountKind, AuditLevel, ConflictSide, FileKind, FileOpKind, FileOpStatus,
            FileSyncState, PairStatus, RunOutcome, RunTrigger,
        },
        file_entry::FileEntry,
        file_op::FileOp,
        file_side::FileSide,
        id::{AccountTag, AuditTag, ConflictTag, FileOpTag, Id, PairTag, SyncRunTag},
        pair::Pair,
        path::{AbsPath, RelPath},
        primitives::{ContentHash, DriveId, DriveItemId, KeychainRef, Timestamp},
        sync_run::SyncRun,
    };
    use tempfile::TempDir;
    use ulid::Ulid;

    use super::InMemoryStore;

    fn ts(seconds: i64) -> Timestamp {
        Timestamp::from_datetime(Utc.timestamp_opt(seconds, 0).unwrap())
    }

    fn id<T: onesync_protocol::id::IdPrefix>(n: u128) -> Id<T> {
        Id::from_ulid(Ulid::from(n))
    }

    #[derive(Debug, PartialEq)]
    struct ParityObservations {
        active_pair_ids: Vec<String>,
        dirty_paths: Vec<String>,
        unresolved_conflicts: usize,
    }

    #[allow(clippy::too_many_lines)]
    async fn drive(store: &dyn StateStore) -> ParityObservations {
        // Fixed ids
        let acct_id = id::<AccountTag>(1u128 << 64);
        let pair_id = id::<PairTag>(2u128 << 64);
        let run_id = id::<SyncRunTag>(3u128 << 64);
        let op_id = id::<FileOpTag>(4u128 << 64);
        let conflict_id = id::<ConflictTag>(5u128 << 64);
        let audit_id = id::<AuditTag>(6u128 << 64);

        // Account
        let acct = Account {
            id: acct_id,
            kind: AccountKind::Personal,
            upn: "alice@example.com".into(),
            tenant_id: "tid".into(),
            drive_id: DriveId::new("drv"),
            display_name: "Alice".into(),
            keychain_ref: KeychainRef::new("kc"),
            scopes: vec!["Files.ReadWrite".into()],
            created_at: ts(1_700_000_000),
            updated_at: ts(1_700_000_000),
        };
        store.account_upsert(&acct).await.unwrap();

        // Pair
        let pair = Pair {
            id: pair_id,
            account_id: acct_id,
            local_path: "/tmp/onedrive".parse::<AbsPath>().unwrap(),
            remote_item_id: DriveItemId::new("root"),
            remote_path: "/".into(),
            display_name: "OneDrive".into(),
            status: PairStatus::Active,
            paused: false,
            delta_token: None,
            errored_reason: None,
            created_at: ts(1_700_000_000),
            updated_at: ts(1_700_000_000),
            last_sync_at: None,
            conflict_count: 0,
            webhook_enabled: false,
        };
        store.pair_upsert(&pair).await.unwrap();

        // A second pair with Removed status (must be excluded from pairs_active)
        let removed_pair = Pair {
            id: id::<PairTag>(9u128 << 64),
            account_id: acct_id,
            local_path: "/tmp/removed".parse::<AbsPath>().unwrap(),
            remote_item_id: DriveItemId::new("root2"),
            remote_path: "/removed".into(),
            display_name: "Removed".into(),
            status: PairStatus::Removed,
            paused: false,
            delta_token: None,
            errored_reason: None,
            created_at: ts(1_700_000_000),
            updated_at: ts(1_700_000_000),
            last_sync_at: None,
            conflict_count: 0,
            webhook_enabled: false,
        };
        store.pair_upsert(&removed_pair).await.unwrap();

        // Three file entries: Clean (excluded from dirty), Dirty, PendingUpload
        let clean_entry = FileEntry {
            pair_id,
            relative_path: "clean.txt".parse::<RelPath>().unwrap(),
            kind: FileKind::File,
            sync_state: FileSyncState::Clean,
            local: None,
            remote: None,
            synced: None,
            pending_op_id: None,
            updated_at: ts(1_700_000_001),
        };
        store.file_entry_upsert(&clean_entry).await.unwrap();

        let dirty_entry = FileEntry {
            pair_id,
            relative_path: "dirty.txt".parse::<RelPath>().unwrap(),
            kind: FileKind::File,
            sync_state: FileSyncState::Dirty,
            local: None,
            remote: None,
            synced: None,
            pending_op_id: None,
            updated_at: ts(1_700_000_002),
        };
        store.file_entry_upsert(&dirty_entry).await.unwrap();

        let pending_entry = FileEntry {
            pair_id,
            relative_path: "pending.txt".parse::<RelPath>().unwrap(),
            kind: FileKind::File,
            sync_state: FileSyncState::PendingUpload,
            local: None,
            remote: None,
            synced: None,
            pending_op_id: None,
            updated_at: ts(1_700_000_003),
        };
        store.file_entry_upsert(&pending_entry).await.unwrap();

        // Sync run
        let run = SyncRun {
            id: run_id,
            pair_id,
            trigger: RunTrigger::Scheduled,
            started_at: ts(1_700_000_002),
            finished_at: Some(ts(1_700_000_003)),
            local_ops: 1,
            remote_ops: 0,
            bytes_uploaded: 100,
            bytes_downloaded: 0,
            outcome: Some(RunOutcome::Success),
            outcome_detail: None,
        };
        store.run_record(&run).await.unwrap();

        // File op: insert then transition to InProgress
        let op = FileOp {
            id: op_id,
            run_id,
            pair_id,
            relative_path: "dirty.txt".parse::<RelPath>().unwrap(),
            kind: FileOpKind::Upload,
            status: FileOpStatus::Enqueued,
            attempts: 0,
            last_error: None,
            metadata: serde_json::Map::default(),
            enqueued_at: ts(1_700_000_002),
            started_at: None,
            finished_at: None,
        };
        store.op_insert(&op).await.unwrap();
        store
            .op_update_status(&op_id, FileOpStatus::InProgress)
            .await
            .unwrap();

        // Conflict (unresolved)
        let file_side = FileSide {
            kind: FileKind::File,
            size_bytes: 10,
            content_hash: Some("00".repeat(32).parse::<ContentHash>().unwrap()),
            mtime: ts(1_700_000_001),
            etag: None,
            remote_item_id: None,
        };
        let conflict = Conflict {
            id: conflict_id,
            pair_id,
            relative_path: "dirty.txt".parse::<RelPath>().unwrap(),
            winner: ConflictSide::Local,
            loser_relative_path: "dirty (conflict).txt".parse::<RelPath>().unwrap(),
            local_side: file_side.clone(),
            remote_side: file_side,
            detected_at: ts(1_700_000_004),
            resolved_at: None,
            resolution: None,
            note: None,
        };
        store.conflict_insert(&conflict).await.unwrap();

        // Audit event
        let evt = AuditEvent {
            id: audit_id,
            ts: ts(1_700_000_005),
            level: AuditLevel::Info,
            kind: "parity.test".into(),
            pair_id: Some(pair_id),
            payload: serde_json::Map::default(),
        };
        store.audit_append(&evt).await.unwrap();

        // Collect observations
        let active_pair_ids = store
            .pairs_active()
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.id.to_string())
            .collect();

        let dirty_paths = store
            .file_entries_dirty(&pair_id, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.relative_path.as_str().to_string())
            .collect();

        let unresolved_conflicts = store.conflicts_unresolved(&pair_id).await.unwrap().len();

        ParityObservations {
            active_pair_ids,
            dirty_paths,
            unresolved_conflicts,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sqlite_and_in_memory_observe_identically() {
        // SqliteStore
        let tmp = TempDir::new().unwrap();
        let pool = open(&tmp.path().join("p.sqlite"), &ts(1_700_000_000)).unwrap();
        let sql = SqliteStore::new(pool);
        let sql_obs = drive(&sql).await;

        // InMemoryStore
        let mem = InMemoryStore::new();
        let mem_obs = drive(&mem).await;

        assert_eq!(sql_obs, mem_obs, "stores must observe identically");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn in_memory_account_round_trips() {
        let store = InMemoryStore::new();
        let acct = Account {
            id: id::<AccountTag>(1u128 << 64),
            kind: AccountKind::Personal,
            upn: "bob@example.com".into(),
            tenant_id: "tid2".into(),
            drive_id: DriveId::new("drv2"),
            display_name: "Bob".into(),
            keychain_ref: KeychainRef::new("kc2"),
            scopes: vec![],
            created_at: ts(1_700_000_000),
            updated_at: ts(1_700_000_000),
        };
        store.account_upsert(&acct).await.unwrap();
        let back = store.account_get(&acct.id).await.unwrap().unwrap();
        assert_eq!(back, acct);
        let missing = store
            .account_get(&id::<AccountTag>(999u128 << 64))
            .await
            .unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn in_memory_pairs_active_excludes_removed() {
        let store = InMemoryStore::new();
        let acct_id = id::<AccountTag>(1u128 << 64);

        let mut pair = Pair {
            id: id::<PairTag>(2u128 << 64),
            account_id: acct_id,
            local_path: "/tmp/a".parse::<AbsPath>().unwrap(),
            remote_item_id: DriveItemId::new("r1"),
            remote_path: "/".into(),
            display_name: "A".into(),
            status: PairStatus::Active,
            paused: false,
            delta_token: None,
            errored_reason: None,
            created_at: ts(1_700_000_000),
            updated_at: ts(1_700_000_000),
            last_sync_at: None,
            conflict_count: 0,
            webhook_enabled: false,
        };
        store.pair_upsert(&pair).await.unwrap();
        assert_eq!(store.pairs_active().await.unwrap().len(), 1);

        pair.status = PairStatus::Removed;
        store.pair_upsert(&pair).await.unwrap();
        assert!(store.pairs_active().await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn in_memory_file_entries_dirty_limit_and_sort() {
        let store = InMemoryStore::new();
        let pair_id = id::<PairTag>(2u128 << 64);

        for i in 0u32..5 {
            let entry = FileEntry {
                pair_id,
                relative_path: format!("file{i}.txt").parse::<RelPath>().unwrap(),
                kind: FileKind::File,
                sync_state: FileSyncState::Dirty,
                local: None,
                remote: None,
                synced: None,
                pending_op_id: None,
                // Decreasing timestamps to test sort order
                updated_at: ts(1_700_000_010 - i64::from(i)),
            };
            store.file_entry_upsert(&entry).await.unwrap();
        }

        // With limit=2, should get the 2 entries with oldest updated_at
        let dirty = store.file_entries_dirty(&pair_id, 2).await.unwrap();
        assert_eq!(dirty.len(), 2);
        // Sorted ascending by updated_at: file4 (t-4) then file3 (t-3)
        assert_eq!(dirty[0].relative_path.as_str(), "file4.txt");
        assert_eq!(dirty[1].relative_path.as_str(), "file3.txt");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn in_memory_op_update_status_silently_noop_on_missing() {
        let store = InMemoryStore::new();
        let missing_id = id::<FileOpTag>(999u128 << 64);
        // Should succeed silently (matching SqliteStore UPDATE-0-rows behaviour)
        store
            .op_update_status(&missing_id, FileOpStatus::InProgress)
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn in_memory_conflicts_unresolved_excludes_resolved() {
        let store = InMemoryStore::new();
        let acct_id = id::<AccountTag>(1u128 << 64);
        let pair_id = id::<PairTag>(2u128 << 64);

        // Need account + pair for SqliteStore FK; InMemoryStore doesn't enforce FK
        let file_side = FileSide {
            kind: FileKind::File,
            size_bytes: 0,
            content_hash: None,
            mtime: ts(1_700_000_000),
            etag: None,
            remote_item_id: None,
        };

        let open_conflict = Conflict {
            id: id::<ConflictTag>(10u128 << 64),
            pair_id,
            relative_path: "a.txt".parse::<RelPath>().unwrap(),
            winner: ConflictSide::Local,
            loser_relative_path: "a (conflict).txt".parse::<RelPath>().unwrap(),
            local_side: file_side.clone(),
            remote_side: file_side.clone(),
            detected_at: ts(1_700_000_001),
            resolved_at: None,
            resolution: None,
            note: None,
        };
        let resolved_conflict = Conflict {
            id: id::<ConflictTag>(11u128 << 64),
            pair_id,
            relative_path: "b.txt".parse::<RelPath>().unwrap(),
            winner: ConflictSide::Remote,
            loser_relative_path: "b (conflict).txt".parse::<RelPath>().unwrap(),
            local_side: file_side.clone(),
            remote_side: file_side,
            detected_at: ts(1_700_000_002),
            resolved_at: Some(ts(1_700_000_003)),
            resolution: Some(onesync_protocol::enums::ConflictResolution::Manual),
            note: None,
        };
        let _ = acct_id; // Not needed for InMemoryStore
        store.conflict_insert(&open_conflict).await.unwrap();
        store.conflict_insert(&resolved_conflict).await.unwrap();

        let unresolved = store.conflicts_unresolved(&pair_id).await.unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].id, open_conflict.id);
    }
}
