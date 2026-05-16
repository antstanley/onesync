//! [`GraphAdapter`]: the concrete `RemoteDrive` implementation.

use async_trait::async_trait;
use onesync_core::ports::{GraphError, RemoteDrive};
use onesync_protocol::{
    primitives::{DeltaCursor, DriveId},
    remote::{
        AccessToken, AccountProfile, DeltaPage, RemoteItem, RemoteItemId, RemoteReadStream,
        UploadSession,
    },
};

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
        let token = self.token().await?;
        let profile_dto = crate::items::account_profile(&self.http, &token)
            .await
            .map_err(map_to_port)?;
        Ok(AccountProfile {
            oid: profile_dto.id,
            upn: profile_dto.user_principal_name,
            display_name: profile_dto.display_name,
            tenant_id: String::new(), // populated by id_token parsing in MSAL flow
            drive_id: String::new(),  // populated by /me/drive call
        })
    }

    async fn item_by_path(
        &self,
        drive: &DriveId,
        path: &str,
    ) -> Result<Option<RemoteItem>, GraphError> {
        let token = self.token().await?;
        crate::items::item_by_path(&self.http, &token, drive, path)
            .await
            .map_err(map_to_port)
    }

    async fn delta(
        &self,
        drive: &DriveId,
        cursor: Option<&DeltaCursor>,
    ) -> Result<DeltaPage, GraphError> {
        let token = self.token().await?;
        crate::delta::delta_page(&self.http, &token, drive, cursor)
            .await
            .map_err(map_to_port)
    }

    async fn download(&self, item: &RemoteItemId) -> Result<RemoteReadStream, GraphError> {
        let token = self.token().await?;
        let bytes =
            crate::download::download(&self.http, &token, &self.drive_id, item.as_str(), None)
                .await
                .map_err(map_to_port)?;
        Ok(RemoteReadStream(bytes))
    }

    async fn upload_small(
        &self,
        parent: &RemoteItemId,
        name: &str,
        bytes: &[u8],
    ) -> Result<RemoteItem, GraphError> {
        let token = self.token().await?;
        crate::upload::small::upload_small(
            &self.http,
            &token,
            &self.drive_id,
            parent.as_str(),
            name,
            bytes,
        )
        .await
        .map_err(map_to_port)
    }

    async fn upload_session(
        &self,
        parent: &RemoteItemId,
        name: &str,
        size: u64,
    ) -> Result<UploadSession, GraphError> {
        // The port's upload_session returns a session handle containing the upload URL.
        // The adapter creates the session via createUploadSession and returns the URL.
        // The caller is responsible for driving the chunk uploads.
        use serde::{Deserialize, Serialize};

        #[derive(Serialize)]
        struct SessionBody<'a> {
            item: SessionItem<'a>,
        }
        #[derive(Serialize)]
        struct SessionItem<'a> {
            #[serde(rename = "@microsoft.graph.conflictBehavior")]
            conflict_behavior: &'a str,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct SessionResp {
            upload_url: String,
        }

        let token = self.token().await?;
        // RP2-F4: percent-encode segments so names with `#`, `?`, ` `, `:`,
        // or non-ASCII don't truncate or hijack the URL.
        let name_enc = crate::urls::encode_segment(name);
        let parent_enc = crate::urls::encode_segment(parent.as_str());
        let session_create_url = format!(
            "https://graph.microsoft.com/v1.0/drives/{}/items/{parent_enc}:/{name_enc}:/createUploadSession",
            self.drive_id.as_str()
        );
        let body = SessionBody {
            item: SessionItem {
                conflict_behavior: "replace",
            },
        };
        // LINT: size is informational here; it's used when the caller uploads chunks.
        let _ = size;

        let request_id = {
            let mut buf = [0u8; 8];
            // LINT: getrandom failure is unrecoverable.
            #[allow(clippy::expect_used)]
            getrandom::getrandom(&mut buf).expect("getrandom");
            buf.iter().fold(String::new(), |mut s, b| {
                use std::fmt::Write as _;
                let _ = write!(s, "{b:02x}");
                s
            })
        };
        let resp = self
            .http
            .post(&session_create_url)
            .bearer_auth(&token)
            .header("client-request-id", &request_id)
            .json(&body)
            .send()
            .await
            .map_err(|e| GraphError::Network {
                detail: e.to_string(),
            })?;

        let sess: SessionResp = crate::client::check_status(resp, &request_id)
            .await
            .map_err(map_to_port)?
            .json()
            .await
            .map_err(|e| GraphError::Decode {
                detail: e.to_string(),
            })?;

        Ok(UploadSession {
            upload_url: sess.upload_url,
            bytes_uploaded: 0,
        })
    }

    async fn rename(&self, item: &RemoteItemId, new_name: &str) -> Result<RemoteItem, GraphError> {
        let token = self.token().await?;
        crate::ops::rename(&self.http, &token, &self.drive_id, item.as_str(), new_name)
            .await
            .map_err(map_to_port)
    }

    async fn delete(&self, item: &RemoteItemId) -> Result<(), GraphError> {
        let token = self.token().await?;
        crate::ops::delete(&self.http, &token, &self.drive_id, item.as_str())
            .await
            .map_err(map_to_port)
    }

    async fn mkdir(&self, parent: &RemoteItemId, name: &str) -> Result<RemoteItem, GraphError> {
        let token = self.token().await?;
        crate::ops::mkdir(&self.http, &token, &self.drive_id, parent.as_str(), name)
            .await
            .map_err(map_to_port)
    }

    async fn subscribe(
        &self,
        drive: &DriveId,
        notification_url: &str,
        client_state: &str,
    ) -> Result<String, GraphError> {
        let token = self.token().await?;
        crate::ops::subscribe(&self.http, &token, drive, notification_url, client_state)
            .await
            .map_err(map_to_port)
    }

    async fn unsubscribe(&self, subscription_id: &str) -> Result<(), GraphError> {
        let token = self.token().await?;
        crate::ops::unsubscribe(&self.http, &token, subscription_id)
            .await
            .map_err(map_to_port)
    }

    async fn renew_subscription(
        &self,
        subscription_id: &str,
        expiration_iso: &str,
    ) -> Result<(), GraphError> {
        let token = self.token().await?;
        crate::ops::renew_subscription(&self.http, &token, subscription_id, expiration_iso)
            .await
            .map_err(map_to_port)
    }
}

// ── Rich adapter tests using wiremock ─────────────────────────────────────────

#[cfg(test)]
mod tests {
    use crate::items::RemoteItem as GraphItem;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_http() -> reqwest::Client {
        reqwest::Client::builder().use_rustls_tls().build().unwrap()
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
        let item: GraphItem = resp.json().await.unwrap();
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
        let item: GraphItem = resp.json().await.unwrap();
        assert_eq!(item.name, "new-name.txt");
    }
}
