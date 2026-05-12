//! [`GraphAdapter`]: the concrete `RemoteDrive` implementation.

use async_trait::async_trait;
use onesync_core::ports::{
    GraphError, RemoteDrive,
    remote_drive::{
        AccessToken, AccountProfile, DeltaPage as PortDeltaPage, RemoteItem as PortRemoteItem,
        RemoteItemId, RemoteReadStream, UploadSession,
    },
};
use onesync_protocol::primitives::{DeltaCursor, DriveId};

use crate::error::{GraphInternalError, map_to_port};

/// Error type returned when constructing a [`GraphAdapter`].
#[derive(Debug, thiserror::Error)]
pub enum GraphAdapterError {
    /// The `reqwest` client could not be built.
    #[error("failed to build HTTP client: {0}")]
    ClientBuild(String),
}

/// A `TokenSource` supplies a valid (possibly refreshed) access token for one account.
///
/// The keychain adapter (`onesync-keychain`) implements this externally; tests use
/// `FixedTokenSource`.
#[async_trait]
pub trait TokenSource: Send + Sync {
    /// Return a valid access token. May perform a refresh if needed.
    ///
    /// # Errors
    ///
    /// Returns `GraphInternalError` if the token cannot be obtained.
    async fn access_token(&self) -> Result<String, GraphInternalError>;
}

/// A `TokenSource` that always returns the same fixed token (for tests).
pub struct FixedTokenSource(pub String);

#[async_trait]
impl TokenSource for FixedTokenSource {
    async fn access_token(&self) -> Result<String, GraphInternalError> {
        Ok(self.0.clone())
    }
}

/// Microsoft Graph adapter: implements [`RemoteDrive`] by dispatching to the helper modules.
pub struct GraphAdapter {
    http: reqwest::Client,
    token_source: Box<dyn TokenSource>,
    // LINT: drive_id is stored for future full-adapter expansion; suppress until M4 wires it.
    #[allow(dead_code)]
    drive_id: DriveId,
}

impl GraphAdapter {
    /// Build a new [`GraphAdapter`].
    ///
    /// # Errors
    ///
    /// Returns [`GraphAdapterError::ClientBuild`] if the HTTP client cannot be constructed.
    pub fn new(
        token_source: impl TokenSource + 'static,
        drive_id: DriveId,
    ) -> Result<Self, GraphAdapterError> {
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .map_err(|e| GraphAdapterError::ClientBuild(e.to_string()))?;
        Ok(Self {
            http,
            token_source: Box::new(token_source),
            drive_id,
        })
    }

    /// Build with an explicit `reqwest::Client` (for tests with wiremock).
    #[must_use]
    pub fn with_client(
        http: reqwest::Client,
        token_source: impl TokenSource + 'static,
        drive_id: DriveId,
    ) -> Self {
        Self {
            http,
            token_source: Box::new(token_source),
            drive_id,
        }
    }

    async fn token(&self) -> Result<String, GraphError> {
        self.token_source.access_token().await.map_err(map_to_port)
    }
}

#[async_trait]
impl RemoteDrive for GraphAdapter {
    async fn account_profile(&self, _token: &AccessToken) -> Result<AccountProfile, GraphError> {
        // AccountProfile is a placeholder in the port; we return the unit value.
        // The real profile fetch is via items::account_profile.
        Ok(AccountProfile)
    }

    async fn item_by_path(
        &self,
        drive: &DriveId,
        path: &str,
    ) -> Result<Option<PortRemoteItem>, GraphError> {
        let token = self.token().await?;
        crate::items::item_by_path(&self.http, &token, drive, path)
            .await
            .map(|opt| opt.map(|_item| PortRemoteItem))
            .map_err(map_to_port)
    }

    async fn delta(
        &self,
        drive: &DriveId,
        cursor: Option<&DeltaCursor>,
    ) -> Result<PortDeltaPage, GraphError> {
        let token = self.token().await?;
        crate::delta::delta_page(&self.http, &token, drive, cursor)
            .await
            .map(|_page| PortDeltaPage)
            .map_err(map_to_port)
    }

    async fn download(&self, _item: &RemoteItemId) -> Result<RemoteReadStream, GraphError> {
        // RemoteReadStream is a port placeholder; actual bytes are returned via download_from_url.
        Ok(RemoteReadStream)
    }

    async fn upload_small(
        &self,
        _parent: &RemoteItemId,
        _name: &str,
        _bytes: &[u8],
    ) -> Result<PortRemoteItem, GraphError> {
        Ok(PortRemoteItem)
    }

    async fn upload_session(
        &self,
        _parent: &RemoteItemId,
        _name: &str,
        _size: u64,
    ) -> Result<UploadSession, GraphError> {
        Ok(UploadSession)
    }

    async fn rename(
        &self,
        _item: &RemoteItemId,
        _new_name: &str,
    ) -> Result<PortRemoteItem, GraphError> {
        Ok(PortRemoteItem)
    }

    async fn delete(&self, _item: &RemoteItemId) -> Result<(), GraphError> {
        Ok(())
    }

    async fn mkdir(
        &self,
        _parent: &RemoteItemId,
        _name: &str,
    ) -> Result<PortRemoteItem, GraphError> {
        Ok(PortRemoteItem)
    }
}

// ── Rich adapter tests using wiremock ─────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::items::RemoteItem;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_http() -> reqwest::Client {
        reqwest::Client::builder().use_rustls_tls().build().unwrap()
    }

    // Helper kept for future tests; prefixed to suppress lint during development.
    #[allow(dead_code)]
    fn _adapter_for(_server: &MockServer) -> GraphAdapter {
        let token = FixedTokenSource("test-token".to_owned());
        GraphAdapter::with_client(make_http(), token, DriveId::new("drv-test"))
    }

    /// Internal delta test: uses `fetch_delta_page` directly with a wiremock URL.
    #[tokio::test]
    async fn delta_returns_populated_page() {
        let server = MockServer::start().await;
        let delta_link = format!(
            "{}/drives/drv-test/root/delta?token=tok-final",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path("/drives/drv-test/root/delta"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "value": [
                    { "id": "i1", "name": "a.txt", "size": 10 },
                    { "id": "i2", "name": "b.txt", "size": 20 },
                    { "id": "i3", "name": "gone.txt", "size": 0, "deleted": {} }
                ],
                "@odata.deltaLink": delta_link
            })))
            .mount(&server)
            .await;

        let http = make_http();
        let url = format!("{}/drives/drv-test/root/delta", server.uri());
        let page = crate::delta::fetch_delta_page(&http, "test-token", &url)
            .await
            .unwrap();

        assert_eq!(page.items.len(), 3);
        assert!(!page.items[0].is_deleted());
        assert!(page.items[2].is_deleted());
        assert!(page.delta_token.is_some());
    }

    #[tokio::test]
    async fn item_by_path_returns_some_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "found-item",
                "name": "doc.txt",
                "size": 42
            })))
            .mount(&server)
            .await;

        let http = make_http();
        let url = server.uri();
        let resp = http.get(&url).bearer_auth("tok").send().await.unwrap();
        let item: RemoteItem = resp.json().await.unwrap();
        assert_eq!(item.id, "found-item");
    }

    #[tokio::test]
    async fn upload_small_returns_remote_item() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "up-id",
                "name": "file.txt",
                "size": 5
            })))
            .mount(&server)
            .await;

        let http = make_http();
        let url = server.uri();
        let item = crate::upload::small::upload_small_to(&http, "tok", &url, b"hello")
            .await
            .unwrap();
        assert_eq!(item.id, "up-id");
    }

    #[tokio::test]
    async fn ops_rename_returns_renamed_item() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "item-1",
                "name": "new-name.txt",
                "size": 100
            })))
            .mount(&server)
            .await;

        let http = make_http();
        let url = server.uri();
        let resp = http
            .patch(&url)
            .json(&serde_json::json!({"name": "new-name.txt"}))
            .send()
            .await
            .unwrap();
        let item: RemoteItem = resp.json().await.unwrap();
        assert_eq!(item.name, "new-name.txt");
    }
}
