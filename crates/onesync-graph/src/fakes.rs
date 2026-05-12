//! In-memory `FakeRemoteDrive` for engine tests (M4).
//!
//! Stores items in a `HashMap` keyed by item id. Generates monotonic fake delta
//! cursors. Mirrors the behavioural contract of `GraphAdapter`:
//!
//! - `delta(None)` → all items + a fresh cursor.
//! - `delta(Some(cursor))` → items changed since that cursor.
//! - `upload_*` → inserts/replaces item; returns its `RemoteItem`.
//! - `download` → returns stored bytes or `NotFound`.
//! - `rename`/`delete`/`mkdir` → mutate the map.

#![cfg(any(test, feature = "fakes"))]

use std::collections::HashMap;

use bytes::Bytes;

use crate::items::{FileFacet, FileHashes, FolderFacet, RemoteItem};

use crate::error::GraphInternalError;

/// An item stored in the `FakeRemoteDrive`.
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
pub struct FakeRemoteDrive {
    items: HashMap<String, FakeItem>,
    generation: u64,
}

impl FakeRemoteDrive {
    /// Create an empty fake drive.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: HashMap::new(),
            generation: 0,
        }
    }

    /// Return all items as a delta page (initial scan, no cursor).
    #[must_use]
    pub fn delta_all(&self) -> (Vec<RemoteItem>, String) {
        let items: Vec<RemoteItem> = self.items.values().map(|fi| fi.meta.clone()).collect();
        let cursor = format!("cursor-gen-{}", self.generation);
        (items, cursor)
    }

    /// Return items changed since `gen_cursor` (where cursor = `"cursor-gen-{N}"`).
    #[must_use]
    pub fn delta_since(&self, cursor: &str) -> (Vec<RemoteItem>, String) {
        let since: u64 = cursor
            .strip_prefix("cursor-gen-")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let items: Vec<RemoteItem> = self
            .items
            .values()
            .filter(|fi| fi.generation > since)
            .map(|fi| fi.meta.clone())
            .collect();
        let new_cursor = format!("cursor-gen-{}", self.generation);
        (items, new_cursor)
    }

    const fn bump(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Insert or replace a file item; returns its metadata.
    pub fn upload(&mut self, parent_id: &str, name: &str, data: Bytes) -> RemoteItem {
        let current_gen = self.bump();
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
        self.items.insert(
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
    pub fn download(&self, item_id: &str) -> Result<Bytes, GraphInternalError> {
        self.items
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
    pub fn rename(
        &mut self,
        item_id: &str,
        new_name: &str,
    ) -> Result<RemoteItem, GraphInternalError> {
        let fi = self
            .items
            .get_mut(item_id)
            .ok_or_else(|| GraphInternalError::NotFound {
                request_id: String::new(),
            })?;
        let new_gen = self.generation + 1;
        self.generation = new_gen;
        new_name.clone_into(&mut fi.meta.name);
        fi.generation = new_gen;
        Ok(fi.meta.clone())
    }

    /// Delete an item.
    ///
    /// # Errors
    ///
    /// Returns [`GraphInternalError::NotFound`] if the item doesn't exist.
    pub fn delete(&mut self, item_id: &str) -> Result<(), GraphInternalError> {
        self.items
            .remove(item_id)
            .map(|_| ())
            .ok_or_else(|| GraphInternalError::NotFound {
                request_id: String::new(),
            })
    }

    /// Create a child folder.
    pub fn mkdir(&mut self, parent_id: &str, name: &str) -> RemoteItem {
        let current_gen = self.bump();
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
        self.items.insert(
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

impl Default for FakeRemoteDrive {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_then_download_round_trip() {
        let mut drive = FakeRemoteDrive::new();
        let data = Bytes::from_static(b"hello world");
        let item = drive.upload("root", "hello.txt", data.clone());
        assert_eq!(item.name, "hello.txt");

        let fetched = drive.download(&item.id).unwrap();
        assert_eq!(fetched, data);
    }

    #[test]
    fn mkdir_then_delta_contains_folder() {
        let mut drive = FakeRemoteDrive::new();
        let folder = drive.mkdir("root", "Documents");
        let (items, _cursor) = drive.delta_all();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, folder.id);
        assert!(items[0].folder.is_some());
    }

    #[test]
    fn rename_changes_name() {
        let mut drive = FakeRemoteDrive::new();
        let item = drive.upload("root", "old.txt", Bytes::from_static(b"data"));
        let renamed = drive.rename(&item.id, "new.txt").unwrap();
        assert_eq!(renamed.name, "new.txt");
    }

    #[test]
    fn delete_removes_item() {
        let mut drive = FakeRemoteDrive::new();
        let item = drive.upload("root", "gone.txt", Bytes::from_static(b"bye"));
        drive.delete(&item.id).unwrap();
        let err = drive.download(&item.id).unwrap_err();
        assert!(matches!(err, GraphInternalError::NotFound { .. }));
    }

    #[test]
    fn delta_since_cursor_returns_only_new_items() {
        let mut drive = FakeRemoteDrive::new();
        drive.upload("root", "a.txt", Bytes::from_static(b"a"));
        let (_items, cursor) = drive.delta_all();

        // Add another item after the cursor.
        drive.upload("root", "b.txt", Bytes::from_static(b"b"));

        let (new_items, _) = drive.delta_since(&cursor);
        assert_eq!(
            new_items.len(),
            1,
            "only the new item should appear since cursor"
        );
        assert_eq!(new_items[0].name, "b.txt");
    }
}
