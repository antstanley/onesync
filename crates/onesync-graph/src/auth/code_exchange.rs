//! Authorization-code grant: `POST /token` with `code` + PKCE verifier.

use serde::{Deserialize, Serialize};

use crate::error::GraphInternalError;

/// Response from the Microsoft identity platform `/token` endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TokenResponse {
    /// Short-lived access token for Graph API calls.
    pub access_token: String,
    /// Refresh token for obtaining new access tokens (Microsoft rotates these).
    pub refresh_token: String,
    /// JWT identity token containing user claims.
    pub id_token: String,
    /// Validity period in seconds.
    pub expires_in: u64,
    /// Scopes granted (space-separated).
    pub scope: String,
}

/// Exchange an authorization code + PKCE verifier for an access + refresh token.
///
/// # Errors
///
/// - [`GraphInternalError::ReAuthRequired`] on `invalid_grant` (code expired / reused).
/// - [`GraphInternalError::Decode`] if the response body is not the expected JSON shape.
/// - [`GraphInternalError::Network`] on transport failures.
pub async fn exchange(
    http: &reqwest::Client,
    authority: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    pkce_verifier: &str,
) -> Result<TokenResponse, GraphInternalError> {
    let url = format!("https://login.microsoftonline.com/{authority}/oauth2/v2.0/token");

    let params = [
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", pkce_verifier),
        ("grant_type", "authorization_code"),
        ("redirect_uri", redirect_uri),
    ];

    let resp =
        http.post(&url)
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
            detail: format!("token endpoint returned 400: {body}"),
        });
    }

    if !status.is_success() {
        return Err(GraphInternalError::Transient {
            detail: format!("token endpoint returned {status}: {body}"),
            request_id: String::new(),
        });
    }

    serde_json::from_str::<TokenResponse>(&body).map_err(|e| GraphInternalError::Decode {
        detail: format!("token response parse failed: {e}: {body}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn exchange_200_happy_path() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "access_token": "at-123",
            "refresh_token": "rt-456",
            "id_token": "header.payload.sig",
            "expires_in": 3600,
            "scope": "Files.ReadWrite offline_access User.Read"
        });
        Mock::given(method("POST"))
            .and(path("/consumers/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&body))
            .mount(&server)
            .await;

        // Override the authority URL to point to wiremock.
        let http = reqwest::Client::builder().use_rustls_tls().build().unwrap();
        // Build a fake "authority" that routes to wiremock by intercepting via base URL.
        // We must rewrite the URL; use a helper.
        let result = exchange_with_base(
            &http,
            &server.uri(),
            "consumers",
            "client-id",
            "auth-code",
            "http://localhost:1234/callback",
            "pkce-verifier",
        )
        .await
        .unwrap();

        assert_eq!(result.access_token, "at-123");
        assert_eq!(result.refresh_token, "rt-456");
        assert_eq!(result.expires_in, 3600);
    }

    #[tokio::test]
    async fn exchange_invalid_grant_returns_re_auth() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "error": "invalid_grant",
            "error_description": "The provided authorization code is expired."
        });
        Mock::given(method("POST"))
            .and(path("/consumers/oauth2/v2.0/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(&body))
            .mount(&server)
            .await;

        let http = reqwest::Client::builder().use_rustls_tls().build().unwrap();
        let err = exchange_with_base(
            &http,
            &server.uri(),
            "consumers",
            "client-id",
            "stale-code",
            "http://localhost:1234/callback",
            "pkce-verifier",
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, GraphInternalError::ReAuthRequired { .. }),
            "expected ReAuthRequired, got: {err:?}"
        );
    }

    /// Test-only version that accepts a base URL so we can point at `wiremock`.
    pub async fn exchange_with_base(
        http: &reqwest::Client,
        base: &str,
        authority: &str,
        client_id: &str,
        code: &str,
        redirect_uri: &str,
        pkce_verifier: &str,
    ) -> Result<TokenResponse, GraphInternalError> {
        let url = format!("{base}/{authority}/oauth2/v2.0/token");
        let params = [
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", pkce_verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri),
        ];
        let resp = http.post(&url).form(&params).send().await.map_err(|e| {
            GraphInternalError::Network {
                detail: e.to_string(),
            }
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
                detail: format!("token endpoint returned 400: {body}"),
            });
        }

        if !status.is_success() {
            return Err(GraphInternalError::Transient {
                detail: format!("token endpoint returned {status}: {body}"),
                request_id: String::new(),
            });
        }

        serde_json::from_str::<TokenResponse>(&body).map_err(|e| GraphInternalError::Decode {
            detail: format!("token response parse failed: {e}: {body}"),
        })
    }
}
