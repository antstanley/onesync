//! Remote drive types shared between `onesync-core` (port trait) and
//! `onesync-graph` (adapter implementation).
//!
//! These types correspond to the Microsoft Graph `/delta` and `driveItem` shapes.
//! They live in `onesync-protocol` so both the core port trait and the graph adapter
//! can depend on them without violating the hexagonal architecture rule (core must not
//! depend on adapters).

use serde::{Deserialize, Serialize};

use crate::primitives::DeltaCursor;

// ── Remote item types ────────────────────────────────────────────────────────

/// File hash values from the `file.hashes` facet of a Graph `driveItem`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileHashes {
    /// SHA-1 hash hex string (Personal accounts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1_hash: Option<String>,
    /// `QuickXorHash` base64 string (Business accounts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quick_xor_hash: Option<String>,
}

/// File facet on a `driveItem`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FileFacet {
    /// Hash values for the file content.
    #[serde(default)]
    pub hashes: FileHashes,
}

/// Folder facet on a `driveItem`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderFacet {
    /// Number of direct children in the folder.
    #[serde(default)]
    pub child_count: u64,
}

/// Deleted facet: present on tombstones in delta responses.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeletedFacet {}

/// Parent reference for a `driveItem`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParentReference {
    /// Parent item identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Drive identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drive_id: Option<String>,
    /// Path within the drive (e.g. `"/drive/root:/Documents"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// A `driveItem` from the Microsoft Graph API.
///
/// Used in `/delta` responses (may be a tombstone) and in single-item lookups.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteItem {
    /// Stable item identifier.
    pub id: String,
    /// File or folder name.
    pub name: String,
    /// Size in bytes (0 for folders or tombstones).
    #[serde(default)]
    pub size: u64,
    /// Entity tag for optimistic concurrency.
    #[serde(rename = "eTag", skip_serializing_if = "Option::is_none")]
    pub e_tag: Option<String>,
    /// Content tag.
    #[serde(rename = "cTag", skip_serializing_if = "Option::is_none")]
    pub c_tag: Option<String>,
    /// Last-modified timestamp (ISO-8601).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified_date_time: Option<String>,
    /// Present if the item is a folder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<FolderFacet>,
    /// Present if the item is a file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileFacet>,
    /// Present when the item is a tombstone (deleted item in a delta response).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<DeletedFacet>,
    /// Parent reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_reference: Option<ParentReference>,
}

impl RemoteItem {
    /// Returns `true` if this is a tombstone (deleted item in a delta response).
    #[must_use]
    pub const fn is_deleted(&self) -> bool {
        self.deleted.is_some()
    }

    /// Returns `true` if this item is a folder.
    #[must_use]
    pub const fn is_folder(&self) -> bool {
        self.folder.is_some()
    }
}

// ── Delta page ───────────────────────────────────────────────────────────────

/// A single page from a Microsoft Graph `/delta` response.
///
/// The caller follows `next_link` until it is `None`; the terminal page carries
/// `delta_token` which must be persisted as the next sync cursor.
#[derive(Debug)]
pub struct DeltaPage {
    /// Drive items changed since the last cursor (may include tombstones).
    pub items: Vec<RemoteItem>,
    /// If `Some`, fetch this URL to continue paging.
    pub next_link: Option<String>,
    /// Present only on the final page; persist as the next delta cursor.
    pub delta_token: Option<DeltaCursor>,
}

// ── Opaque handle types ──────────────────────────────────────────────────────

/// An issued OAuth 2.0 access token. Opaque to the engine; the graph adapter
/// constructs and uses it. The engine only passes it through.
#[derive(Clone, Debug)]
pub struct AccessToken(pub String);

impl AccessToken {
    /// Return the token string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The `/me` profile fetched after a successful login. The engine uses the
/// display name and UPN; the adapter populates all fields.
#[derive(Clone, Debug)]
pub struct AccountProfile {
    /// Microsoft Object ID for the signed-in user.
    pub oid: String,
    /// User Principal Name (email address).
    pub upn: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Azure AD tenant identifier (used to determine `AccountKind`).
    pub tenant_id: String,
    /// `OneDrive` drive identifier.
    pub drive_id: String,
}

/// Stable identifier for a remote drive item.
///
/// Wraps the Graph item id string. Opaque to the engine; passed through.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RemoteItemId(pub String);

impl RemoteItemId {
    /// Return the underlying id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A streaming response from a Graph download.
///
/// Wraps a `bytes::Bytes` payload eagerly loaded for the current adapter.
/// A future streaming variant may replace this once the engine supports it.
pub struct RemoteReadStream(pub bytes::Bytes);

/// A Graph resumable upload session handle.
///
/// Contains the upload URL and the byte range already uploaded.
#[derive(Debug)]
pub struct UploadSession {
    /// The URL to PUT/POST chunks to.
    pub upload_url: String,
    /// Number of bytes already uploaded (for resume after 416).
    pub bytes_uploaded: u64,
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_item_is_deleted_tombstone() {
        let item = RemoteItem {
            id: "i1".to_owned(),
            name: "gone.txt".to_owned(),
            size: 0,
            e_tag: None,
            c_tag: None,
            last_modified_date_time: None,
            folder: None,
            file: None,
            deleted: Some(DeletedFacet {}),
            parent_reference: None,
        };
        assert!(item.is_deleted());
        assert!(!item.is_folder());
    }

    #[test]
    fn remote_item_is_folder() {
        let item = RemoteItem {
            id: "f1".to_owned(),
            name: "Documents".to_owned(),
            size: 0,
            e_tag: None,
            c_tag: None,
            last_modified_date_time: None,
            folder: Some(FolderFacet { child_count: 3 }),
            file: None,
            deleted: None,
            parent_reference: None,
        };
        assert!(!item.is_deleted());
        assert!(item.is_folder());
    }

    #[test]
    fn remote_item_round_trips_through_json() {
        let json = serde_json::json!({
            "id": "item-1",
            "name": "file.txt",
            "size": 1024,
            "eTag": "etag-abc",
            "file": { "hashes": { "sha1Hash": "da39a3ee5e6b4b0d3255bfef95601890afd80709" } }
        });
        let item: RemoteItem = serde_json::from_value(json).expect("deserialises");
        assert_eq!(item.id, "item-1");
        assert_eq!(item.size, 1024);
        assert!(item.file.is_some());
    }

    #[test]
    fn access_token_as_str_returns_inner() {
        let tok = AccessToken("bearer-xyz".to_owned());
        assert_eq!(tok.as_str(), "bearer-xyz");
    }
}
