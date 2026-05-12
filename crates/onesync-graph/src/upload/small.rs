//! Single-PUT upload for files at or below `GRAPH_SMALL_UPLOAD_MAX_BYTES` (4 MiB).

use onesync_protocol::primitives::DriveId;

use crate::error::GraphInternalError;
use crate::items::RemoteItem;

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Upload a small file (≤ 4 MiB) via a single `PUT` request.
///
/// Uses `?@microsoft.graph.conflictBehavior=replace`.
///
/// # Errors
///
/// - [`GraphInternalError::InvalidArgument`] if `bytes.len() > GRAPH_SMALL_UPLOAD_MAX_BYTES`.
/// - [`GraphInternalError::Network`] on transport failure.
/// - [`GraphInternalError::Decode`] if the response cannot be parsed as a `RemoteItem`.
pub async fn upload_small(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    parent_item_id: &str,
    name: &str,
    bytes: &[u8],
) -> Result<RemoteItem, GraphInternalError> {
    use onesync_core::limits::GRAPH_SMALL_UPLOAD_MAX_BYTES;
    if bytes.len() as u64 > GRAPH_SMALL_UPLOAD_MAX_BYTES {
        return Err(GraphInternalError::InvalidArgument {
            detail: format!(
                "payload {} bytes exceeds small-upload limit of {GRAPH_SMALL_UPLOAD_MAX_BYTES}",
                bytes.len()
            ),
        });
    }

    let url = format!(
        "{GRAPH_BASE}/drives/{}/items/{parent_item_id}:/{name}:/content?@microsoft.graph.conflictBehavior=replace",
        drive_id.as_str()
    );
    upload_small_to(http, token, &url, bytes).await
}

/// Upload bytes to an explicit URL (for tests).
pub async fn upload_small_to(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    bytes: &[u8],
) -> Result<RemoteItem, GraphInternalError> {
    let request_id = new_request_id();
    let resp = http
        .put(url)
        .bearer_auth(token)
        .header("client-request-id", &request_id)
        .header("Content-Type", "application/octet-stream")
        .body(bytes.to_vec())
        .send()
        .await
        .map_err(|e| GraphInternalError::Network {
            detail: e.to_string(),
        })?;

    crate::client::check_status(resp, &request_id)
        .await?
        .json::<RemoteItem>()
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

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client() -> reqwest::Client {
        reqwest::Client::builder().use_rustls_tls().build().unwrap()
    }

    #[tokio::test]
    async fn upload_small_200_returns_remote_item() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/upload"))
            .and(header("content-type", "application/octet-stream"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "new-item-id",
                "name": "hello.txt",
                "size": 5
            })))
            .mount(&server)
            .await;

        let http = make_client();
        let url = format!("{}/upload", server.uri());
        let item = upload_small_to(&http, "tok", &url, b"hello").await.unwrap();
        assert_eq!(item.id, "new-item-id");
        assert_eq!(item.name, "hello.txt");
    }

    #[tokio::test]
    async fn upload_small_too_large_returns_invalid_argument() {
        let http = make_client();
        // LINT: 4MiB+1 always fits in usize on 64-bit targets; allowing truncation lint here.
        #[allow(clippy::cast_possible_truncation)]
        let big = vec![0u8; (onesync_core::limits::GRAPH_SMALL_UPLOAD_MAX_BYTES + 1) as usize];
        let drive_id = DriveId::new("drv1");
        let err = upload_small(&http, "tok", &drive_id, "parent-id", "big.bin", &big)
            .await
            .unwrap_err();
        assert!(
            matches!(err, GraphInternalError::InvalidArgument { .. }),
            "expected InvalidArgument, got: {err:?}"
        );
    }
}
