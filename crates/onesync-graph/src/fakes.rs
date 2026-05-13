//! In-memory `FakeRemoteDrive` for engine tests.
//!
//! Stores items in a `HashMap` keyed by item id. Generates monotonic fake delta
//! cursors. Mirrors the behavioural contract of `GraphAdapter`:
//!
//! - `delta(None)` → all items + a fresh cursor.
//! - `delta(Some(cursor))` → items changed since that cursor.
//! - `upload_*` → inserts/replaces item; returns its `RemoteItem`.
//! - `download` → returns stored bytes or `NotFound`.
//! - `rename`/`delete`/`mkdir` → mutate the map.

#[cfg(any(test, feature = "fakes"))]
use std::collections::HashMap;

#[cfg(any(test, feature = "fakes"))]
use bytes::Bytes;

#[cfg(any(test, feature = "fakes"))]
use async_trait::async_trait;

#[cfg(any(test, feature = "fakes"))]
use onesync_core::ports::{GraphError, RemoteDrive};

#[cfg(any(test, feature = "fakes"))]
use onesync_protocol::{
    primitives::{DeltaCursor, DriveId},
    remote::{
        AccessToken, AccountProfile, DeltaPage, FileFacet, FileHashes, FolderFacet, RemoteItem,
        RemoteItemId, RemoteReadStream, UploadSession,
    },
};

#[cfg(any(test, feature = "fakes"))]
use crate::error::GraphInternalError;

/// An item stored in the `FakeRemoteDrive`.
#[cfg(any(test, feature = "fakes"))]
#[derive(Clone, Debug)]
pub struct FakeItem {
    /// The graph metadata for the item.
    pub meta: RemoteItem,
    /// Stored file bytes (`None` for folders).
    pub bytes: Option<Bytes>,
    /// The generation at which this item was last modified.
    pub generation: u64,
}

/// In-memory implementation of `RemoteDrive` semantics.
#[cfg(any(test, feature = "fakes"))]
pub struct FakeRemoteDrive {
    items: std::sync::Mutex<HashMap<String, FakeItem>>,
    generation: std::sync::atomic::AtomicU64,
}

#[cfg(any(test, feature = "fakes"))]
impl FakeRemoteDrive {
    /// Create an empty fake drive.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: std::sync::Mutex::new(HashMap::new()),
            generation: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn bump_gen(&self) -> u64 {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    fn current_gen(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Return all items as a delta page (initial scan, no cursor).
    #[must_use]
    pub fn delta_all_sync(&self) -> (Vec<RemoteItem>, String) {
        // LINT: test helper; lock unwrap is acceptable.
        #[allow(clippy::unwrap_used)]
        let items: Vec<RemoteItem> = self
            .items
            .lock()
            .unwrap()
            .values()
            .map(|fi| fi.meta.clone())
            .collect();
        let cursor = format!("cursor-gen-{}", self.current_gen());
        (items, cursor)
    }

    /// Return items changed since `gen_cursor` (where cursor = `"cursor-gen-{N}"`).
    #[must_use]
    pub fn delta_since_sync(&self, cursor: &str) -> (Vec<RemoteItem>, String) {
        let since: u64 = cursor
            .strip_prefix("cursor-gen-")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        // LINT: test helper; lock unwrap is acceptable.
        #[allow(clippy::unwrap_used)]
        let items: Vec<RemoteItem> = self
            .items
            .lock()
            .unwrap()
            .values()
            .filter(|fi| fi.generation > since)
            .map(|fi| fi.meta.clone())
            .collect();
        let new_cursor = format!("cursor-gen-{}", self.current_gen());
        (items, new_cursor)
    }

    /// Insert or replace a file item; returns its metadata.
    pub fn upload_sync(&self, parent_id: &str, name: &str, data: Bytes) -> RemoteItem {
        let current_gen = self.bump_gen();
        let id = format!("{parent_id}/{name}");
        let size = data.len() as u64;
        let sha1 = {
            use sha1::{Digest, Sha1};
            let mut h = Sha1::new();
            h.update(&data);
            h.finalize().iter().fold(String::new(), |mut s, b| {
                use std::fmt::Write as _;
                let _ = write!(s, "{b:02x}");
                s
            })
        };
        let meta = RemoteItem {
            id: id.clone(),
            name: name.to_owned(),
            size,
            e_tag: Some(format!("etag-{current_gen}")),
            c_tag: None,
            last_modified_date_time: None,
            folder: None,
            file: Some(FileFacet {
                hashes: FileHashes {
                    sha1_hash: Some(sha1),
                    quick_xor_hash: None,
                },
            }),
            deleted: None,
            parent_reference: None,
        };
        // LINT: test helper; lock unwrap is acceptable.
        #[allow(clippy::unwrap_used)]
        self.items.lock().unwrap().insert(
            id,
            FakeItem {
                meta: meta.clone(),
                bytes: Some(data),
                generation: current_gen,
            },
        );
        meta
    }

    /// Download stored bytes by item id.
    ///
    /// # Errors
    ///
    /// Returns [`GraphInternalError::NotFound`] if the item doesn't exist.
    pub fn download_sync(&self, item_id: &str) -> Result<Bytes, GraphInternalError> {
        // LINT: test helper; lock unwrap is acceptable.
        #[allow(clippy::unwrap_used)]
        self.items
            .lock()
            .unwrap()
            .get(item_id)
            .and_then(|fi| fi.bytes.clone())
            .ok_or_else(|| GraphInternalError::NotFound {
                request_id: String::new(),
            })
    }

    /// Rename an item.
    ///
    /// # Errors
    ///
    /// Returns [`GraphInternalError::NotFound`] if the item doesn't exist.
    // LINT: significant_drop_tightening — items_map is dropped at block end; no contention
    // concern in test helpers.
    #[allow(clippy::significant_drop_tightening)]
    pub fn rename_sync(
        &self,
        item_id: &str,
        new_name: &str,
    ) -> Result<RemoteItem, GraphInternalError> {
        let new_gen = self.bump_gen();
        // LINT: test helper; lock unwrap is acceptable.
        #[allow(clippy::unwrap_used)]
        let mut items_map = self.items.lock().unwrap();
        let fi = items_map
            .get_mut(item_id)
            .ok_or_else(|| GraphInternalError::NotFound {
                request_id: String::new(),
            })?;
        new_name.clone_into(&mut fi.meta.name);
        fi.generation = new_gen;
        Ok(fi.meta.clone())
    }

    /// Delete an item.
    ///
    /// # Errors
    ///
    /// Returns [`GraphInternalError::NotFound`] if the item doesn't exist.
    pub fn delete_sync(&self, item_id: &str) -> Result<(), GraphInternalError> {
        // LINT: test helper; lock unwrap is acceptable.
        #[allow(clippy::unwrap_used)]
        self.items
            .lock()
            .unwrap()
            .remove(item_id)
            .map(|_| ())
            .ok_or_else(|| GraphInternalError::NotFound {
                request_id: String::new(),
            })
    }

    /// Create a child folder.
    pub fn mkdir_sync(&self, parent_id: &str, name: &str) -> RemoteItem {
        let current_gen = self.bump_gen();
        let id = format!("{parent_id}/{name}");
        let meta = RemoteItem {
            id: id.clone(),
            name: name.to_owned(),
            size: 0,
            e_tag: Some(format!("etag-{current_gen}")),
            c_tag: None,
            last_modified_date_time: None,
            folder: Some(FolderFacet { child_count: 0 }),
            file: None,
            deleted: None,
            parent_reference: None,
        };
        // LINT: test helper; lock unwrap is acceptable.
        #[allow(clippy::unwrap_used)]
        self.items.lock().unwrap().insert(
            id,
            FakeItem {
                meta: meta.clone(),
                bytes: None,
                generation: current_gen,
            },
        );
        meta
    }
}

#[cfg(any(test, feature = "fakes"))]
impl Default for FakeRemoteDrive {
    fn default() -> Self {
        Self::new()
    }
}

/// Implement `RemoteDrive` for `FakeRemoteDrive` so engine tests can use it directly.
#[cfg(any(test, feature = "fakes"))]
#[async_trait]
impl RemoteDrive for FakeRemoteDrive {
    async fn account_profile(&self, _token: &AccessToken) -> Result<AccountProfile, GraphError> {
        Ok(AccountProfile {
            oid: "fake-oid".to_owned(),
            upn: "fake@example.com".to_owned(),
            display_name: "Fake User".to_owned(),
            tenant_id: "fake-tenant".to_owned(),
            drive_id: "fake-drive".to_owned(),
        })
    }

    async fn item_by_path(
        &self,
        _drive: &DriveId,
        path: &str,
    ) -> Result<Option<RemoteItem>, GraphError> {
        // LINT: test helper; lock unwrap is acceptable.
        #[allow(clippy::unwrap_used)]
        let items_map = self.items.lock().unwrap();
        Ok(items_map
            .values()
            .find(|fi| fi.meta.name == path || fi.meta.id == path)
            .map(|fi| fi.meta.clone()))
    }

    async fn delta(
        &self,
        _drive: &DriveId,
        cursor: Option<&DeltaCursor>,
    ) -> Result<DeltaPage, GraphError> {
        let (items, new_cursor_str) = cursor.map_or_else(
            || self.delta_all_sync(),
            |c| self.delta_since_sync(c.as_str()),
        );
        Ok(DeltaPage {
            items,
            next_link: None,
            delta_token: Some(DeltaCursor::new(new_cursor_str)),
        })
    }

    async fn download(&self, item: &RemoteItemId) -> Result<RemoteReadStream, GraphError> {
        self.download_sync(item.as_str())
            .map(RemoteReadStream)
            .map_err(crate::error::map_to_port)
    }

    async fn upload_small(
        &self,
        parent: &RemoteItemId,
        name: &str,
        bytes: &[u8],
    ) -> Result<RemoteItem, GraphError> {
        Ok(self.upload_sync(parent.as_str(), name, Bytes::copy_from_slice(bytes)))
    }

    async fn upload_session(
        &self,
        parent: &RemoteItemId,
        name: &str,
        _size: u64,
    ) -> Result<UploadSession, GraphError> {
        // LINT: fake upload URL; not used in tests that only check the handle.
        Ok(UploadSession {
            upload_url: format!("fake://upload/{}/{}", parent.as_str(), name),
            bytes_uploaded: 0,
        })
    }

    async fn rename(&self, item: &RemoteItemId, new_name: &str) -> Result<RemoteItem, GraphError> {
        self.rename_sync(item.as_str(), new_name)
            .map_err(crate::error::map_to_port)
    }

    async fn delete(&self, item: &RemoteItemId) -> Result<(), GraphError> {
        self.delete_sync(item.as_str())
            .map_err(crate::error::map_to_port)
    }

    async fn subscribe(
        &self,
        _drive: &DriveId,
        _notification_url: &str,
        client_state: &str,
    ) -> Result<String, GraphError> {
        // Fake: return a deterministic id derived from the client_state so tests can assert.
        Ok(format!("fake-sub-{client_state}"))
    }

    async fn unsubscribe(&self, _subscription_id: &str) -> Result<(), GraphError> {
        Ok(())
    }

    async fn mkdir(&self, parent: &RemoteItemId, name: &str) -> Result<RemoteItem, GraphError> {
        Ok(self.mkdir_sync(parent.as_str(), name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_then_download_round_trip() {
        let drive = FakeRemoteDrive::new();
        let data = Bytes::from_static(b"hello world");
        let item = drive.upload_sync("root", "hello.txt", data.clone());
        assert_eq!(item.name, "hello.txt");

        let fetched = drive.download_sync(&item.id).unwrap();
        assert_eq!(fetched, data);
    }

    #[test]
    fn mkdir_then_delta_contains_folder() {
        let drive = FakeRemoteDrive::new();
        let folder = drive.mkdir_sync("root", "Documents");
        let (items, _cursor) = drive.delta_all_sync();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, folder.id);
        assert!(items[0].folder.is_some());
    }

    #[test]
    fn rename_changes_name() {
        let drive = FakeRemoteDrive::new();
        let item = drive.upload_sync("root", "old.txt", Bytes::from_static(b"data"));
        let renamed = drive.rename_sync(&item.id, "new.txt").unwrap();
        assert_eq!(renamed.name, "new.txt");
    }

    #[test]
    fn delete_removes_item() {
        let drive = FakeRemoteDrive::new();
        let item = drive.upload_sync("root", "gone.txt", Bytes::from_static(b"bye"));
        drive.delete_sync(&item.id).unwrap();
        let err = drive.download_sync(&item.id).unwrap_err();
        assert!(matches!(err, GraphInternalError::NotFound { .. }));
    }

    #[test]
    fn delta_since_cursor_returns_only_new_items() {
        let drive = FakeRemoteDrive::new();
        drive.upload_sync("root", "a.txt", Bytes::from_static(b"a"));
        let (_items, cursor) = drive.delta_all_sync();

        // Add another item after the cursor.
        drive.upload_sync("root", "b.txt", Bytes::from_static(b"b"));

        let (new_items, _) = drive.delta_since_sync(&cursor);
        assert_eq!(
            new_items.len(),
            1,
            "only the new item should appear since cursor"
        );
        assert_eq!(new_items[0].name, "b.txt");
    }

    #[tokio::test]
    async fn implements_remote_drive_trait() {
        let drive = FakeRemoteDrive::new();
        let drive_id = DriveId::new("fake-drive");
        let token = AccessToken("tok".to_owned());

        // account_profile
        let profile = drive.account_profile(&token).await.unwrap();
        assert_eq!(profile.upn, "fake@example.com");

        // delta (initial)
        let page = drive.delta(&drive_id, None).await.unwrap();
        assert!(page.items.is_empty());
        assert!(page.delta_token.is_some());

        // upload_small
        let parent_id = RemoteItemId("root".to_owned());
        let item = drive
            .upload_small(&parent_id, "test.txt", b"content")
            .await
            .unwrap();
        assert_eq!(item.name, "test.txt");

        // download
        let rs = drive
            .download(&RemoteItemId(item.id.clone()))
            .await
            .unwrap();
        assert_eq!(&rs.0[..], b"content");

        // rename
        let renamed = drive
            .rename(&RemoteItemId(item.id.clone()), "renamed.txt")
            .await
            .unwrap();
        assert_eq!(renamed.name, "renamed.txt");

        // delete
        drive.delete(&RemoteItemId(item.id.clone())).await.unwrap();

        // mkdir
        let folder = drive.mkdir(&parent_id, "NewFolder").await.unwrap();
        assert_eq!(folder.name, "NewFolder");
        assert!(folder.folder.is_some());
    }
}
