//! Shared helpers for `onesync-core` integration tests.
//!
//! Provides minimal in-memory fakes for `RemoteDrive`, `AuditSink`, and a
//! `FakeContext` that wires all fakes + `EngineDeps` together.

#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use onesync_core::engine::cycle::EngineDeps;
use onesync_core::ports::{
    AuditSink, Clock, GraphError, RemoteDrive, StateStore,
    remote_drive::{
        AccessToken, AccountProfile, DeltaPage, RemoteItem, RemoteItemId, RemoteReadStream,
        UploadSession,
    },
};
use onesync_fs_local::fakes::InMemoryLocalFs;
use onesync_protocol::{
    account::Account,
    audit::AuditEvent,
    enums::{AccountKind, PairStatus},
    id::{AccountId, PairId},
    pair::Pair,
    primitives::{DeltaCursor, DriveId, DriveItemId, KeychainRef},
};
use onesync_state::fakes::InMemoryStore;
use onesync_time::fakes::{FakeJitter, TestClock, TestIdGenerator};

// ── Minimal RemoteDrive fake ─────────────────────────────────────────────────

/// A `RemoteDrive` that returns empty delta pages and no-ops all writes.
pub struct NoopRemoteDrive {
    /// If `Some(err)`, the next call to `delta` will return that error.
    pub delta_error: Mutex<Option<GraphError>>,
}

impl NoopRemoteDrive {
    pub const fn new() -> Self {
        Self {
            delta_error: Mutex::new(None),
        }
    }

    pub fn inject_delta_error(&self, err: GraphError) {
        *self.delta_error.lock().expect("lock") = Some(err);
    }
}

#[async_trait]
impl RemoteDrive for NoopRemoteDrive {
    async fn account_profile(&self, _: &AccessToken) -> Result<AccountProfile, GraphError> {
        Ok(AccountProfile)
    }

    async fn item_by_path(&self, _: &DriveId, _: &str) -> Result<Option<RemoteItem>, GraphError> {
        Ok(None)
    }

    async fn delta(
        &self,
        _drive: &DriveId,
        _cursor: Option<&DeltaCursor>,
    ) -> Result<DeltaPage, GraphError> {
        let pending_err = self.delta_error.lock().expect("lock").take();
        if let Some(err) = pending_err {
            return Err(err);
        }
        Ok(DeltaPage)
    }

    async fn download(&self, _: &RemoteItemId) -> Result<RemoteReadStream, GraphError> {
        Ok(RemoteReadStream)
    }

    async fn upload_small(
        &self,
        _: &RemoteItemId,
        _: &str,
        _: &[u8],
    ) -> Result<RemoteItem, GraphError> {
        Ok(RemoteItem)
    }

    async fn upload_session(
        &self,
        _: &RemoteItemId,
        _: &str,
        _: u64,
    ) -> Result<UploadSession, GraphError> {
        Ok(UploadSession)
    }

    async fn rename(&self, _: &RemoteItemId, _: &str) -> Result<RemoteItem, GraphError> {
        Ok(RemoteItem)
    }

    async fn delete(&self, _: &RemoteItemId) -> Result<(), GraphError> {
        Ok(())
    }

    async fn mkdir(&self, _: &RemoteItemId, _: &str) -> Result<RemoteItem, GraphError> {
        Ok(RemoteItem)
    }
}

// ── Minimal AuditSink fake ───────────────────────────────────────────────────

/// A `AuditSink` that captures events in a Vec.
#[derive(Default)]
pub struct CapturingAuditSink {
    events: Mutex<Vec<AuditEvent>>,
}

impl CapturingAuditSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<AuditEvent> {
        self.events.lock().expect("lock").clone()
    }
}

impl AuditSink for CapturingAuditSink {
    fn emit(&self, event: AuditEvent) {
        self.events.lock().expect("lock").push(event);
    }
}

// ── FakeContext ──────────────────────────────────────────────────────────────

/// All fakes wired together for engine integration tests.
pub struct FakeContext {
    pub store: Arc<InMemoryStore>,
    pub local: Arc<InMemoryLocalFs>,
    pub remote: Arc<NoopRemoteDrive>,
    pub clock: Arc<TestClock>,
    pub ids: Arc<TestIdGenerator>,
    pub jitter: Arc<FakeJitter>,
    pub audit: Arc<CapturingAuditSink>,
}

impl FakeContext {
    pub fn new() -> Self {
        let clock = TestClock::at(Utc.with_ymd_and_hms(2026, 5, 12, 10, 0, 0).unwrap());
        Self {
            store: Arc::new(InMemoryStore::new()),
            local: Arc::new(InMemoryLocalFs::new()),
            remote: Arc::new(NoopRemoteDrive::new()),
            clock: Arc::new(clock),
            ids: Arc::new(TestIdGenerator::seeded(42)),
            jitter: Arc::new(FakeJitter(0.0)),
            audit: Arc::new(CapturingAuditSink::new()),
        }
    }

    /// Build an `EngineDeps` borrowing from `self`.
    pub fn deps(&self) -> EngineDeps<'_, TestIdGenerator> {
        EngineDeps {
            state: self.store.as_ref(),
            local: self.local.as_ref(),
            remote: self.remote.as_ref(),
            clock: self.clock.as_ref(),
            ids: self.ids.as_ref(),
            audit: self.audit.as_ref(),
            jitter: self.jitter.as_ref(),
            host: "test-host".to_owned(),
        }
    }

    /// Seed a minimal active pair with a delta cursor already set.
    /// Returns `(PairId, Pair)`.
    pub async fn seed_pair(&self) -> (PairId, Pair) {
        use onesync_protocol::id::{AccountTag, Id, PairTag};
        use ulid::Ulid;

        let account_id = AccountId::from(Id::<AccountTag>::from_ulid(Ulid::from(1u128)));
        let pair_id = Id::<PairTag>::from_ulid(Ulid::from(2u128));

        let account = Account {
            id: account_id,
            kind: AccountKind::Personal,
            upn: "test@example.com".into(),
            tenant_id: "tenant-0".into(),
            drive_id: DriveId::new("drive-0"),
            display_name: "Test Account".into(),
            keychain_ref: KeychainRef::new("kc-0"),
            scopes: vec![],
            created_at: self.clock.now(),
            updated_at: self.clock.now(),
        };
        self.store.account_upsert(&account).await.expect("account");

        let pair = Pair {
            id: pair_id,
            account_id,
            local_path: "/tmp/sync".parse().expect("path"),
            remote_item_id: DriveItemId::new("root"),
            remote_path: "/".into(),
            display_name: "Test Pair".into(),
            status: PairStatus::Active,
            paused: false,
            delta_token: Some(DeltaCursor::new("cursor-0")),
            errored_reason: None,
            created_at: self.clock.now(),
            updated_at: self.clock.now(),
            last_sync_at: None,
            conflict_count: 0,
        };
        self.store.pair_upsert(&pair).await.expect("pair");
        (pair_id, pair)
    }
}
