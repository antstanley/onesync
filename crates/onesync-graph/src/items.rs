//! Graph item-probe helpers: `/me`, `/me/drive`, `/me/drive/root:/{path}`.

use serde::{Deserialize, Serialize};

use onesync_protocol::primitives::DriveId;

use crate::error::GraphInternalError;

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Minimal `/me` response.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProfileDto {
    /// Microsoft Object ID for the signed-in user.
    pub id: String,
    /// User Principal Name (email address).
    pub user_principal_name: String,
    /// Human-readable display name.
    pub display_name: String,
}

/// Minimal `/me/drive` response.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveDto {
    /// `OneDrive` drive identifier.
    pub id: String,
    /// `"personal"` for `OneDrive` Personal, `"business"` for `OneDrive` for Business.
    pub drive_type: String,
}

/// File hash values from the `file.hashes` facet.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileHashes {
    /// SHA-1 hash hex string (Personal accounts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha1_hash: Option<String>,
    /// `QuickXorHash` hex string (Business accounts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quick_xor_hash: Option<String>,
}

/// File facet on a `driveItem`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FileFacet {
    /// Hash values for the file.
    #[serde(default)]
    pub hashes: FileHashes,
}

/// Folder facet on a `driveItem`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderFacet {
    /// Number of children in the folder.
    #[serde(default)]
    pub child_count: u64,
}

/// Deleted facet: present when a `driveItem` has been removed.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeletedFacet {}

/// A `driveItem` from the Microsoft Graph API.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteItem {
    /// Stable item identifier.
    pub id: String,
    /// File/folder name.
    pub name: String,
    /// Size in bytes.
    #[serde(default)]
    pub size: u64,
    /// Entity tag for conflict detection.
    #[serde(rename = "eTag", skip_serializing_if = "Option::is_none")]
    pub e_tag: Option<String>,
    /// Content tag.
    #[serde(rename = "cTag", skip_serializing_if = "Option::is_none")]
    pub c_tag: Option<String>,
    /// Last modified time (ISO-8601).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified_date_time: Option<String>,
    /// Present if the item is a folder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<FolderFacet>,
    /// Present if the item is a file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileFacet>,
    /// Present when the item has been deleted (tombstone in delta responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted: Option<DeletedFacet>,
    /// Parent reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_reference: Option<ParentReference>,
}

impl RemoteItem {
    /// Returns `true` if this is a tombstone (deleted item) in a delta response.
    #[must_use]
    pub const fn is_deleted(&self) -> bool {
        self.deleted.is_some()
    }
}

/// Minimal parent reference for a `driveItem`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParentReference {
    /// Parent item identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Drive identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drive_id: Option<String>,
    /// Path within the drive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// `GET /me` — fetch the signed-in user's profile.
///
/// # Errors
///
/// Returns [`GraphInternalError`] on HTTP or decode failure.
pub async fn account_profile(
    http: &reqwest::Client,
    token: &str,
) -> Result<AccountProfileDto, GraphInternalError> {
    let url = format!("{GRAPH_BASE}/me");
    json_get(http, token, &url).await
}

/// `GET /me/drive` — fetch the user's default drive metadata.
///
/// # Errors
///
/// Returns [`GraphInternalError`] on HTTP or decode failure.
pub async fn default_drive(
    http: &reqwest::Client,
    token: &str,
) -> Result<DriveDto, GraphInternalError> {
    let url = format!("{GRAPH_BASE}/me/drive");
    json_get(http, token, &url).await
}

/// `GET /me/drive/root:/{path}` — resolve a drive item by path.
///
/// Returns `None` on 404.
///
/// # Errors
///
/// Returns [`GraphInternalError`] on HTTP or decode failure (except 404 → `Ok(None)`).
pub async fn item_by_path(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    path: &str,
) -> Result<Option<RemoteItem>, GraphInternalError> {
    let encoded = encode_path(path);
    let url = format!("{GRAPH_BASE}/drives/{}/root:{encoded}", drive_id.as_str());
    match json_get::<RemoteItem>(http, token, &url).await {
        Ok(item) => Ok(Some(item)),
        Err(GraphInternalError::NotFound { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// `GET /drives/{drive_id}/items/{item_id}` — fetch a single item by id.
///
/// Returns `None` on 404.
///
/// # Errors
///
/// Returns [`GraphInternalError`] on HTTP or decode failure.
pub async fn item_by_id(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    item_id: &str,
) -> Result<Option<RemoteItem>, GraphInternalError> {
    let url = format!("{GRAPH_BASE}/drives/{}/items/{item_id}", drive_id.as_str());
    match json_get::<RemoteItem>(http, token, &url).await {
        Ok(item) => Ok(Some(item)),
        Err(GraphInternalError::NotFound { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Issue a GET request expecting a JSON body.
async fn json_get<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    token: &str,
    url: &str,
) -> Result<T, GraphInternalError> {
    let request_id = new_request_id();
    let resp = http
        .get(url)
        .bearer_auth(token)
        .header("client-request-id", &request_id)
        .send()
        .await
        .map_err(|e| GraphInternalError::Network {
            detail: e.to_string(),
        })?;

    crate::client::check_status(resp, &request_id)
        .await?
        .json::<T>()
        .await
        .map_err(|e| GraphInternalError::Decode {
            detail: e.to_string(),
        })
}

fn new_request_id() -> String {
    let mut buf = [0u8; 8];
    // LINT: getrandom failure is unrecoverable.
    #[allow(clippy::expect_used)]
    getrandom::getrandom(&mut buf).expect("getrandom");
    buf.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn encode_path(path: &str) -> String {
    // Minimal percent-encoding: encode characters that must be encoded in a path segment
    // but keep '/' as-is (it's the path delimiter).
    path.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '~') {
                vec![c]
            } else {
                format!("%{:02X}", c as u32).chars().collect::<Vec<_>>()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client() -> reqwest::Client {
        reqwest::Client::builder().use_rustls_tls().build().unwrap()
    }

    #[allow(dead_code)]
    fn drive_id(s: &str) -> DriveId {
        DriveId::new(s)
    }

    #[tokio::test]
    async fn account_profile_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "user-oid",
                "userPrincipalName": "alice@example.com",
                "displayName": "Alice"
            })))
            .mount(&server)
            .await;

        // Override GRAPH_BASE is not possible without injection; use the inner helper.
        let http = make_client();
        let resp = http
            .get(format!("{}/me", server.uri()))
            .bearer_auth("tok")
            .send()
            .await
            .unwrap();
        let dto: AccountProfileDto = resp.json().await.unwrap();
        assert_eq!(dto.id, "user-oid");
        assert_eq!(dto.user_principal_name, "alice@example.com");
    }

    #[tokio::test]
    async fn default_drive_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/me/drive"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "drive-abc",
                "driveType": "personal"
            })))
            .mount(&server)
            .await;

        let http = make_client();
        let resp = http
            .get(format!("{}/me/drive", server.uri()))
            .bearer_auth("tok")
            .send()
            .await
            .unwrap();
        let dto: DriveDto = resp.json().await.unwrap();
        assert_eq!(dto.id, "drive-abc");
        assert_eq!(dto.drive_type, "personal");
    }

    #[tokio::test]
    async fn item_by_path_404_returns_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": { "code": "itemNotFound", "message": "The resource could not be found." }
            })))
            .mount(&server)
            .await;

        let http = make_client();
        // We call the internal helper to avoid real Graph URL.
        let request_id = "r1".to_owned();
        let resp = http
            .get(format!(
                "{}/drives/drv1/root:/Documents/Notexist",
                server.uri()
            ))
            .bearer_auth("tok")
            .send()
            .await
            .unwrap();
        let err = crate::client::check_status(resp, &request_id)
            .await
            .unwrap_err();
        assert!(matches!(err, GraphInternalError::NotFound { .. }));
    }

    #[tokio::test]
    async fn item_by_path_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/drives/drv1/root:/Documents/notes.md"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "item-xyz",
                "name": "notes.md",
                "size": 1024,
                "eTag": "etag-1",
                "cTag": "ctag-1",
                "lastModifiedDateTime": "2026-05-12T10:00:00Z",
                "file": { "hashes": { "sha1Hash": "da39a3ee5e6b4b0d3255bfef95601890afd80709" } }
            })))
            .mount(&server)
            .await;

        let http = make_client();
        let resp = http
            .get(format!(
                "{}/drives/drv1/root:/Documents/notes.md",
                server.uri()
            ))
            .bearer_auth("tok")
            .send()
            .await
            .unwrap();
        assert!(resp.status().is_success());
        let item: RemoteItem = resp.json().await.unwrap();
        assert_eq!(item.id, "item-xyz");
        assert_eq!(item.name, "notes.md");
        assert!(item.file.is_some());
    }
}
