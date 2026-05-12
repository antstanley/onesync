//! Refresh-token grant: `POST /token` with `refresh_token`.

use crate::auth::code_exchange::TokenResponse;
use crate::error::GraphInternalError;

/// Exchange a refresh token for a new `{ access_token, refresh_token, … }`.
///
/// Microsoft rotates refresh tokens on use — the response contains a fresh
/// `refresh_token` which the caller must persist.
///
/// # Errors
///
/// - [`GraphInternalError::ReAuthRequired`] on `invalid_grant` (token revoked / password changed).
/// - [`GraphInternalError::Decode`] if the response body is malformed.
/// - [`GraphInternalError::Network`] on transport failures.
pub async fn refresh(
    http: &reqwest::Client,
    authority: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse, GraphInternalError> {
    let url = format!("https://login.microsoftonline.com/{authority}/oauth2/v2.0/token");
    exchange_inner(http, &url, client_id, refresh_token).await
}

async fn exchange_inner(
    http: &reqwest::Client,
    url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<TokenResponse, GraphInternalError> {
    let params = [
        ("client_id", client_id),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];

    let resp =
        http.post(url)
            .form(&params)
            .send()
            .await
            .map_err(|e| GraphInternalError::Network {
                detail: e.to_string(),
            })?;

    let status = resp.status();
    let body = resp.text().await.map_err(|e| GraphInternalError::Decode {
        detail: e.to_string(),
    })?;

    if status == reqwest::StatusCode::BAD_REQUEST {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body)
            && v.get("error").and_then(|e| e.as_str()) == Some("invalid_grant")
        {
            return Err(GraphInternalError::ReAuthRequired {
                request_id: String::new(),
            });
        }
        return Err(GraphInternalError::Decode {
            detail: format!("refresh endpoint returned 400: {body}"),
        });
    }

    if !status.is_success() {
        return Err(GraphInternalError::Transient {
            detail: format!("refresh endpoint returned {status}: {body}"),
            request_id: String::new(),
        });
    }

    serde_json::from_str::<TokenResponse>(&body).map_err(|e| GraphInternalError::Decode {
        detail: format!("refresh token response parse failed: {e}: {body}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn refresh_200_happy_path() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "access_token": "new-at",
            "refresh_token": "new-rt",
            "id_token": "h.p.s",
            "expires_in": 3600,
            "scope": "Files.ReadWrite offline_access"
        });
        Mock::given(method("POST"))
            .and(path("/consumers/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        let http = reqwest::Client::builder().use_rustls_tls().build().unwrap();
        let url = format!("{}/consumers/oauth2/v2.0/token", server.uri());
        let result = exchange_inner(&http, &url, "cid", "old-rt").await.unwrap();
        assert_eq!(result.access_token, "new-at");
        assert_eq!(result.refresh_token, "new-rt");
    }

    #[tokio::test]
    async fn refresh_invalid_grant_returns_re_auth() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "error": "invalid_grant",
            "error_description": "Refresh token has expired or the user changed their password."
        });
        Mock::given(method("POST"))
            .and(path("/consumers/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(&body))
            .mount(&server)
            .await;

        let http = reqwest::Client::builder().use_rustls_tls().build().unwrap();
        let url = format!("{}/consumers/oauth2/v2.0/token", server.uri());
        let err = exchange_inner(&http, &url, "cid", "revoked-rt")
            .await
            .unwrap_err();
        assert!(
            matches!(err, GraphInternalError::ReAuthRequired { .. }),
            "expected ReAuthRequired, got: {err:?}"
        );
    }
}
