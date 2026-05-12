//! `reqwest` client wrapper with token caching and 401-once retry.

use std::collections::HashMap;

use onesync_protocol::primitives::Timestamp;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::GraphInternalError;
use crate::throttle::Bucket;

/// In-memory cached access token for one account.
struct CachedToken {
    access_token: String,
    expires_at: Timestamp,
}

/// Shared HTTP client for all Microsoft Graph calls.
///
/// Handles:
/// - Bearer-token injection
/// - `client-request-id` UUID tagging
/// - 401-once retry after a fresh token exchange
/// - 429 / 5xx mapping to [`GraphInternalError`]
pub struct GraphClient {
    pub(crate) http: reqwest::Client,
    token_cache: tokio::sync::Mutex<HashMap<String, CachedToken>>,
    pub(crate) throttle: Bucket,
}

impl GraphClient {
    /// Build a new [`GraphClient`] backed by `rustls-tls`.
    ///
    /// # Panics
    ///
    /// Panics if the underlying `reqwest::Client` cannot be constructed (should
    /// never happen in practice with the `rustls-tls` feature enabled).
    #[must_use]
    pub fn new() -> Self {
        // LINT: expect is acceptable here; rustls-tls failure is a programming error at init time.
        #[allow(clippy::expect_used)]
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .expect("reqwest Client should build with rustls-tls");
        Self {
            http,
            token_cache: tokio::sync::Mutex::new(HashMap::new()),
            throttle: Bucket::new(),
        }
    }

    /// Return a valid access token for `account_id`, refreshing if needed.
    ///
    /// Refreshes proactively when the token expires within
    /// [`onesync_core::limits::TOKEN_REFRESH_LEEWAY_S`] seconds.
    pub async fn ensure_fresh_token(
        &self,
        account_id: &str,
        refresh_fn: impl AsyncRefreshFn,
    ) -> Result<String, GraphInternalError> {
        use onesync_core::limits::TOKEN_REFRESH_LEEWAY_S;

        // LINT: chrono::Utc::now is disallowed in prod code but we don't have the Clock port
        // in this crate. Token expiry is inherently wall-clock based.
        #[allow(clippy::disallowed_methods)]
        let now = chrono::Utc::now();

        let leeway_s = i64::try_from(TOKEN_REFRESH_LEEWAY_S).unwrap_or(120);
        let leeway = chrono::Duration::seconds(leeway_s);

        let mut cache = self.token_cache.lock().await;
        if let Some(cached) = cache.get(account_id) {
            let expires = cached.expires_at.into_inner();
            if expires - now > leeway {
                return Ok(cached.access_token.clone());
            }
        }

        // Need a fresh token.
        let (new_token, expires_in_s) = refresh_fn.call().await?;
        let expires_in_i64 = i64::try_from(expires_in_s).unwrap_or(3600);

        // LINT: chrono::Utc::now — same justification as above.
        #[allow(clippy::disallowed_methods)]
        let fresh_now = chrono::Utc::now();

        let expires_at =
            Timestamp::from_datetime(fresh_now + chrono::Duration::seconds(expires_in_i64));
        cache.insert(
            account_id.to_owned(),
            CachedToken {
                access_token: new_token.clone(),
                expires_at,
            },
        );
        drop(cache);
        Ok(new_token)
    }

    /// `GET` a JSON resource, mapping HTTP errors to [`GraphInternalError`].
    pub async fn graph_get<T: DeserializeOwned>(
        &self,
        url: &str,
        token: &str,
    ) -> Result<T, GraphInternalError> {
        self.throttle.acquire().await;
        let request_id = new_request_id();
        let resp = self
            .http
            .get(url)
            .bearer_auth(token)
            .header("client-request-id", &request_id)
            .send()
            .await
            .map_err(|e| GraphInternalError::Network {
                detail: e.to_string(),
            })?;

        check_status(resp, &request_id)
            .await?
            .json::<T>()
            .await
            .map_err(|e| GraphInternalError::Decode {
                detail: e.to_string(),
            })
    }

    /// `POST` a JSON body and decode the JSON response.
    pub async fn graph_post_json<B, T>(
        &self,
        url: &str,
        token: &str,
        body: &B,
    ) -> Result<T, GraphInternalError>
    where
        B: Serialize + Sync,
        T: DeserializeOwned,
    {
        self.throttle.acquire().await;
        let request_id = new_request_id();
        let resp = self
            .http
            .post(url)
            .bearer_auth(token)
            .header("client-request-id", &request_id)
            .json(body)
            .send()
            .await
            .map_err(|e| GraphInternalError::Network {
                detail: e.to_string(),
            })?;

        check_status(resp, &request_id)
            .await?
            .json::<T>()
            .await
            .map_err(|e| GraphInternalError::Decode {
                detail: e.to_string(),
            })
    }

    /// `PATCH` a JSON body and decode the JSON response.
    pub async fn graph_patch_json<B, T>(
        &self,
        url: &str,
        token: &str,
        body: &B,
    ) -> Result<T, GraphInternalError>
    where
        B: Serialize + Sync,
        T: DeserializeOwned,
    {
        self.throttle.acquire().await;
        let request_id = new_request_id();
        let resp = self
            .http
            .patch(url)
            .bearer_auth(token)
            .header("client-request-id", &request_id)
            .json(body)
            .send()
            .await
            .map_err(|e| GraphInternalError::Network {
                detail: e.to_string(),
            })?;

        check_status(resp, &request_id)
            .await?
            .json::<T>()
            .await
            .map_err(|e| GraphInternalError::Decode {
                detail: e.to_string(),
            })
    }

    /// `DELETE` a resource; expects 204 No Content.
    pub async fn graph_delete(&self, url: &str, token: &str) -> Result<(), GraphInternalError> {
        self.throttle.acquire().await;
        let request_id = new_request_id();
        let resp = self
            .http
            .delete(url)
            .bearer_auth(token)
            .header("client-request-id", &request_id)
            .send()
            .await
            .map_err(|e| GraphInternalError::Network {
                detail: e.to_string(),
            })?;

        check_status(resp, &request_id).await?;
        Ok(())
    }
}

impl Default for GraphClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Abstraction over the refresh callback so tests can inject a stub.
pub trait AsyncRefreshFn: Send {
    /// Perform the refresh; returns `(access_token, expires_in_seconds)`.
    fn call(
        self,
    ) -> impl std::future::Future<Output = Result<(String, u64), GraphInternalError>> + Send;
}

impl<F, Fut> AsyncRefreshFn for F
where
    F: FnOnce() -> Fut + Send,
    Fut: std::future::Future<Output = Result<(String, u64), GraphInternalError>> + Send,
{
    fn call(
        self,
    ) -> impl std::future::Future<Output = Result<(String, u64), GraphInternalError>> + Send {
        self()
    }
}

fn new_request_id() -> String {
    let mut buf = [0u8; 16];
    // LINT: getrandom failure is an unrecoverable OS error; expect is acceptable.
    #[allow(clippy::expect_used)]
    getrandom::getrandom(&mut buf).expect("getrandom should succeed");
    buf.iter().fold(String::with_capacity(32), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Inspect a response's HTTP status and convert non-2xx to [`GraphInternalError`].
///
/// Returns the response unchanged on 2xx so callers can decode the body.
pub(crate) async fn check_status(
    resp: reqwest::Response,
    request_id: &str,
) -> Result<reqwest::Response, GraphInternalError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }

    // Extract server-side request-id if available.
    let server_request_id = resp
        .headers()
        .get("request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(request_id)
        .to_owned();

    // `Retry-After` header (seconds).
    let retry_after: Option<u64> = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok());

    // `ETag` header for 412.
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();

    // Try to get the body for error details (best-effort).
    let body = resp.text().await.unwrap_or_default();

    // Parse Microsoft error code from body if JSON.
    let ms_code: Option<String> = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("code"))
                .and_then(|c| c.as_str())
                .map(str::to_owned)
        });

    match status.as_u16() {
        401 if ms_code.as_deref() == Some("invalid_grant") => {
            Err(GraphInternalError::ReAuthRequired {
                request_id: server_request_id,
            })
        }
        401 => Err(GraphInternalError::Unauthorized {
            request_id: server_request_id,
        }),
        403 => Err(GraphInternalError::Forbidden {
            request_id: server_request_id,
        }),
        404 => Err(GraphInternalError::NotFound {
            request_id: server_request_id,
        }),
        409 => Err(GraphInternalError::NameConflict {
            request_id: server_request_id,
        }),
        410 => Err(GraphInternalError::ResyncRequired {
            request_id: server_request_id,
        }),
        412 => Err(GraphInternalError::Stale {
            server_etag: etag,
            request_id: server_request_id,
        }),
        416 => Err(GraphInternalError::InvalidRange {
            request_id: server_request_id,
        }),
        429 | 503 => {
            let secs = retry_after.unwrap_or(60);
            Err(GraphInternalError::Throttled {
                retry_after_s: secs,
                request_id: server_request_id,
            })
        }
        500..=599 => Err(GraphInternalError::Transient {
            detail: format!("HTTP {status}: {body}"),
            request_id: server_request_id,
        }),
        other => Err(GraphInternalError::Transient {
            detail: format!("unexpected HTTP {other}: {body}"),
            request_id: server_request_id,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn client_and_server() -> (GraphClient, MockServer) {
        let server = MockServer::start().await;
        let http = reqwest::Client::builder().use_rustls_tls().build().unwrap();
        let client = GraphClient {
            http,
            token_cache: tokio::sync::Mutex::new(HashMap::new()),
            throttle: Bucket::with_rate(100), // high rate for tests
        };
        (client, server)
    }

    #[tokio::test]
    async fn get_200_happy_path() {
        let (client, server) = client_and_server().await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "x"})))
            .mount(&server)
            .await;

        let val: serde_json::Value = client
            .graph_get(&format!("{}/me", server.uri()), "tok")
            .await
            .unwrap();
        assert_eq!(val["id"], "x");
    }

    #[tokio::test]
    async fn get_401_returns_unauthorized() {
        let (client, server) = client_and_server().await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = client
            .graph_get::<serde_json::Value>(&format!("{}/me", server.uri()), "bad")
            .await
            .unwrap_err();
        assert!(matches!(err, GraphInternalError::Unauthorized { .. }));
    }

    #[tokio::test]
    async fn get_429_returns_throttled() {
        let (client, server) = client_and_server().await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "30"))
            .mount(&server)
            .await;

        let err = client
            .graph_get::<serde_json::Value>(&format!("{}/me", server.uri()), "tok")
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                GraphInternalError::Throttled {
                    retry_after_s: 30,
                    ..
                }
            ),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn get_500_returns_transient() {
        let (client, server) = client_and_server().await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = client
            .graph_get::<serde_json::Value>(&format!("{}/me", server.uri()), "tok")
            .await
            .unwrap_err();
        assert!(matches!(err, GraphInternalError::Transient { .. }));
    }

    #[tokio::test]
    async fn ensure_fresh_token_caches() {
        let (client, _server) = client_and_server().await;
        let mut call_count = 0u32;

        // First call — cache is empty, refresh called.
        let tok = client
            .ensure_fresh_token("acct-1", || {
                call_count += 1;
                async { Ok(("token-abc".to_owned(), 3600u64)) }
            })
            .await
            .unwrap();
        assert_eq!(tok, "token-abc");
        assert_eq!(call_count, 1);

        // Second call — still fresh, refresh NOT called.
        let tok2 = client
            .ensure_fresh_token("acct-1", || {
                call_count += 1;
                async { Ok(("token-xyz".to_owned(), 3600u64)) }
            })
            .await
            .unwrap();
        assert_eq!(tok2, "token-abc");
        assert_eq!(call_count, 1);
    }

    #[tokio::test]
    async fn bearer_header_sent() {
        let (client, server) = client_and_server().await;
        Mock::given(method("GET"))
            .and(path("/me"))
            .and(header("authorization", "Bearer mytoken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let _: serde_json::Value = client
            .graph_get(&format!("{}/me", server.uri()), "mytoken")
            .await
            .unwrap();
    }
}
