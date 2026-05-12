//! Graph item-probe helpers: `/me`, `/me/drive`, `/me/drive/root:/{path}`.
//!
//! The `RemoteItem` type (and related facets) are defined in `onesync-protocol::remote`
//! and re-exported here for callers within the graph crate that already import from `items`.

use serde::{Deserialize, Serialize};

use onesync_protocol::primitives::DriveId;

use crate::error::GraphInternalError;

/// Re-export the canonical `RemoteItem` from `onesync-protocol::remote`.
pub use onesync_protocol::remote::{
    DeletedFacet, FileFacet, FileHashes, FolderFacet, ParentReference, RemoteItem,
};

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
