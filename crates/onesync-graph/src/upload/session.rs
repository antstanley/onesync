//! Resumable upload session: `createUploadSession` + chunked `PUT` with 416 resume.

use onesync_protocol::primitives::DriveId;
use serde::{Deserialize, Serialize};

use crate::error::GraphInternalError;
use crate::items::RemoteItem;

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

// ── Session-creation request body ─────────────────────────────────────────────

#[derive(Serialize)]
struct SessionBody {
    item: SessionItem,
}

#[derive(Serialize)]
struct SessionItem {
    #[serde(rename = "@microsoft.graph.conflictBehavior")]
    conflict_behavior: &'static str,
}

/// Response from `createUploadSession`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadSessionResponse {
    upload_url: String,
    #[allow(dead_code)]
    expiration_date_time: Option<String>,
}

/// Intermediate response while uploading chunks (202 Accepted).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChunkAck {
    next_expected_ranges: Vec<String>,
}

/// Upload a file using a resumable session for files larger than 4 MiB.
///
/// 1. `POST createUploadSession`.
/// 2. For each chunk (multiples of 320 KiB; `SESSION_CHUNK_BYTES` = 10 MiB):
///    `PUT {uploadUrl}` with `Content-Range`.
/// 3. Handles 416 by re-querying `nextExpectedRanges` and resuming.
///
/// # Errors
///
/// Returns [`GraphInternalError`] on network, 4xx, or decode failures.
pub async fn upload_session(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    parent_item_id: &str,
    name: &str,
    total_size: u64,
    chunks: impl Iterator<Item = bytes::Bytes>,
) -> Result<RemoteItem, GraphInternalError> {
    let session_url = format!(
        "{GRAPH_BASE}/drives/{}/items/{parent_item_id}:/{name}:/createUploadSession",
        drive_id.as_str()
    );
    upload_session_with_base(http, token, &session_url, total_size, chunks).await
}

/// Testable variant accepting an arbitrary session-creation URL.
// LINT: this function handles a multi-phase protocol; splitting would obscure the flow.
#[allow(clippy::too_many_lines)]
pub async fn upload_session_with_base(
    http: &reqwest::Client,
    token: &str,
    session_create_url: &str,
    total_size: u64,
    chunks: impl Iterator<Item = bytes::Bytes>,
) -> Result<RemoteItem, GraphInternalError> {
    // 1. Create the upload session.
    let request_id = new_request_id();
    let body = SessionBody {
        item: SessionItem {
            conflict_behavior: "replace",
        },
    };

    let resp = http
        .post(session_create_url)
        .bearer_auth(token)
        .header("client-request-id", &request_id)
        .json(&body)
        .send()
        .await
        .map_err(|e| GraphInternalError::Network {
            detail: e.to_string(),
        })?;

    let session: UploadSessionResponse = crate::client::check_status(resp, &request_id)
        .await?
        .json()
        .await
        .map_err(|e| GraphInternalError::Decode {
            detail: e.to_string(),
        })?;

    let upload_url = session.upload_url;

    // 2. Upload chunks.
    let all_chunks: Vec<bytes::Bytes> = chunks.collect();
    let mut offset: u64 = 0;
    let mut chunk_index = 0;

    while chunk_index < all_chunks.len() {
        let chunk = &all_chunks[chunk_index];
        let chunk_len = chunk.len() as u64;
        let end = offset + chunk_len - 1;
        let content_range = format!("bytes {offset}-{end}/{total_size}");

        let chunk_rid = new_request_id();
        let resp = http
            .put(&upload_url)
            .header("Content-Range", &content_range)
            .header("Content-Length", chunk_len.to_string())
            .header("client-request-id", &chunk_rid)
            .body(chunk.to_vec())
            .send()
            .await
            .map_err(|e| GraphInternalError::Network {
                detail: e.to_string(),
            })?;

        let status = resp.status();

        if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            // 416: re-query the session to find out what ranges the server still needs.
            let state_resp =
                http.get(&upload_url)
                    .send()
                    .await
                    .map_err(|e| GraphInternalError::Network {
                        detail: e.to_string(),
                    })?;
            let state: ChunkAck =
                state_resp
                    .json()
                    .await
                    .map_err(|e| GraphInternalError::Decode {
                        detail: format!("416 session re-query parse failed: {e}"),
                    })?;
            // Resume from the first expected range.
            if let Some(range_str) = state.next_expected_ranges.first()
                && let Some(new_offset) = parse_range_start(range_str)
            {
                // Fast-forward chunk_index to match the new offset.
                offset = new_offset;
                chunk_index = find_chunk_index(&all_chunks, new_offset);
                continue;
            }
            return Err(GraphInternalError::InvalidRange {
                request_id: chunk_rid,
            });
        }

        if status == reqwest::StatusCode::ACCEPTED {
            // 202: server acknowledged the chunk; continue with next.
            offset += chunk_len;
            chunk_index += 1;
            continue;
        }

        if status == reqwest::StatusCode::OK || status == reqwest::StatusCode::CREATED {
            // Final chunk accepted; parse the RemoteItem.
            let body = resp.text().await.map_err(|e| GraphInternalError::Decode {
                detail: e.to_string(),
            })?;
            return serde_json::from_str::<RemoteItem>(&body).map_err(|e| {
                GraphInternalError::Decode {
                    detail: format!("final upload response parse failed: {e}: {body}"),
                }
            });
        }

        // Any other status is an error.
        let body = resp.text().await.unwrap_or_default();
        return Err(GraphInternalError::Transient {
            detail: format!("unexpected upload chunk status {status}: {body}"),
            request_id: chunk_rid,
        });
    }

    // If we consumed all chunks without getting a 200/201, something went wrong.
    Err(GraphInternalError::Decode {
        detail: "upload session completed chunks without receiving final item".to_owned(),
    })
}

fn parse_range_start(range_str: &str) -> Option<u64> {
    // Format: "start-" or "start-end"
    range_str.split('-').next()?.parse().ok()
}

fn find_chunk_index(chunks: &[bytes::Bytes], target_offset: u64) -> usize {
    let mut off = 0u64;
    for (i, chunk) in chunks.iter().enumerate() {
        if off >= target_offset {
            return i;
        }
        off += chunk.len() as u64;
    }
    chunks.len()
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
    use bytes::Bytes;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client() -> reqwest::Client {
        reqwest::Client::builder().use_rustls_tls().build().unwrap()
    }

    fn make_chunks(total: usize, chunk_size: usize) -> Vec<Bytes> {
        // LINT: i % 256 always fits in u8; cast is safe.
        #[allow(clippy::cast_possible_truncation)]
        let data: Vec<u8> = (0..total).map(|i| (i % 256) as u8).collect();
        data.chunks(chunk_size)
            .map(Bytes::copy_from_slice)
            .collect()
    }

    #[tokio::test]
    async fn upload_session_two_chunks_succeeds() {
        let server = MockServer::start().await;

        // Session creation returns uploadUrl.
        let upload_url = format!("{}/upload", server.uri());
        Mock::given(method("POST"))
            .and(path("/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "uploadUrl": upload_url,
                "expirationDateTime": "2026-05-13T00:00:00Z",
                "nextExpectedRanges": ["0-"]
            })))
            .mount(&server)
            .await;

        // Chunk 1 → 202 Accepted
        Mock::given(method("PUT"))
            .and(path("/upload"))
            .respond_with(ResponseTemplate::new(202).set_body_json(serde_json::json!({
                "nextExpectedRanges": ["5-"]
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // Chunk 2 → 201 Created with RemoteItem
        Mock::given(method("PUT"))
            .and(path("/upload"))
            .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
                "id": "uploaded-item",
                "name": "big.bin",
                "size": 10
            })))
            .mount(&server)
            .await;

        let http = make_client();
        let session_url = format!("{}/session", server.uri());
        let chunks = make_chunks(10, 5);
        let item = upload_session_with_base(&http, "tok", &session_url, 10, chunks.into_iter())
            .await
            .unwrap();

        assert_eq!(item.id, "uploaded-item");
        assert_eq!(item.name, "big.bin");
    }

    #[tokio::test]
    async fn upload_session_416_retry_succeeds() {
        let server = MockServer::start().await;

        let upload_url = format!("{}/upload", server.uri());
        Mock::given(method("POST"))
            .and(path("/session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "uploadUrl": upload_url,
                "expirationDateTime": "2026-05-13T00:00:00Z"
            })))
            .mount(&server)
            .await;

        // First PUT → 416
        Mock::given(method("PUT"))
            .and(path("/upload"))
            .respond_with(ResponseTemplate::new(416))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        // GET session state (re-query after 416)
        Mock::given(method("GET"))
            .and(path("/upload"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "nextExpectedRanges": ["0-"]
            })))
            .mount(&server)
            .await;

        // Retry PUT → 200 final
        Mock::given(method("PUT"))
            .and(path("/upload"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "retried-item",
                "name": "retry.bin",
                "size": 5
            })))
            .mount(&server)
            .await;

        let http = make_client();
        let session_url = format!("{}/session", server.uri());
        let chunks = vec![Bytes::from_static(b"hello")];
        let item = upload_session_with_base(&http, "tok", &session_url, 5, chunks.into_iter())
            .await
            .unwrap();

        assert_eq!(item.id, "retried-item");
    }
}
