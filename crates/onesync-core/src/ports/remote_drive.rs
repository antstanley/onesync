//! `RemoteDrive` port: the `OneDrive` surface the engine drives.

use async_trait::async_trait;
use onesync_protocol::{
    primitives::{DeltaCursor, DriveId},
    remote::{
        AccessToken, AccountProfile, DeltaPage, RemoteItem, RemoteItemId, RemoteReadStream,
        UploadSession,
    },
};

/// Errors returned by `RemoteDrive` operations.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// Server returned 401 even after a token refresh.
    #[error("unauthorized")]
    Unauthorized,
    /// Refresh-token exchange returned `invalid_grant`; the user must re-authenticate.
    #[error("re-authentication required")]
    ReAuthRequired,
    /// Server returned 403.
    #[error("forbidden")]
    Forbidden,
    /// Server returned 404 for the target item.
    #[error("not found")]
    NotFound,
    /// Server returned 409 on a create where the name already exists.
    #[error("name conflict")]
    NameConflict,
    /// Server signalled that the delta cursor is too old and the engine must re-scan.
    #[error("resync required")]
    ResyncRequired,
    /// Server returned 412 because the supplied `ETag` did not match.
    #[error("stale (server etag {server_etag})")]
    Stale {
        /// The current `ETag` returned by the server.
        server_etag: String,
    },
    /// Server returned 416 on a range request.
    #[error("invalid range")]
    InvalidRange,
    /// Server is throttling; retry after the given duration.
    #[error("throttled (retry after {retry_after_s}s)")]
    Throttled {
        /// Seconds to wait before retrying, per the `Retry-After` header.
        retry_after_s: u64,
    },
    /// Transient 5xx without a `Retry-After`.
    #[error("transient: {0}")]
    Transient(String),
    /// Network-layer failure (DNS, TLS, connection reset, timeout).
    #[error("network: {detail}")]
    Network {
        /// Human-readable detail from the underlying error.
        detail: String,
    },
    /// Response body did not match the expected shape.
    #[error("decode: {detail}")]
    Decode {
        /// Description of the decoding failure.
        detail: String,
    },
    /// Downloaded content did not match the server-supplied hash.
    #[error("hash mismatch")]
    HashMismatch,
    /// Upload was refused because the file exceeds `MAX_FILE_SIZE_BYTES`.
    #[error("file too large")]
    TooLarge,
}

/// The Microsoft Graph surface the engine drives.
#[async_trait]
pub trait RemoteDrive: Send + Sync {
    /// Fetch the signed-in user's profile.
    async fn account_profile(&self, token: &AccessToken) -> Result<AccountProfile, GraphError>;
    /// Resolve a folder by its absolute path within the drive.
    async fn item_by_path(
        &self,
        drive: &DriveId,
        path: &str,
    ) -> Result<Option<RemoteItem>, GraphError>;
    /// Page through changes since `cursor`, or full inventory if `cursor` is `None`.
    async fn delta(
        &self,
        drive: &DriveId,
        cursor: Option<&DeltaCursor>,
    ) -> Result<DeltaPage, GraphError>;
    /// Begin a streaming download of an item by id.
    async fn download(&self, item: &RemoteItemId) -> Result<RemoteReadStream, GraphError>;
    /// Single-PUT upload for files at or below `GRAPH_SMALL_UPLOAD_MAX_BYTES`.
    async fn upload_small(
        &self,
        parent: &RemoteItemId,
        name: &str,
        bytes: &[u8],
    ) -> Result<RemoteItem, GraphError>;
    /// Open a resumable upload session for larger files.
    async fn upload_session(
        &self,
        parent: &RemoteItemId,
        name: &str,
        size: u64,
    ) -> Result<UploadSession, GraphError>;
    /// Rename a remote item.
    async fn rename(&self, item: &RemoteItemId, new_name: &str) -> Result<RemoteItem, GraphError>;
    /// Move a remote item to the Recycle Bin.
    async fn delete(&self, item: &RemoteItemId) -> Result<(), GraphError>;
    /// Create a child folder under `parent`.
    async fn mkdir(&self, parent: &RemoteItemId, name: &str) -> Result<RemoteItem, GraphError>;
}
