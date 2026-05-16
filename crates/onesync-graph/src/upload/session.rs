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
    // RP2-F4: percent-encode segments so special characters don't break the URL.
    let name_enc = crate::urls::encode_segment(name);
    let parent_enc = crate::urls::encode_segment(parent_item_id);
    let session_url = format!(
        "{GRAPH_BASE}/drives/{}/items/{parent_enc}:/{name_enc}:/createUploadSession",
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
    // RP2-F1: `chunk_start` tracks the absolute byte offset of
    // `all_chunks[chunk_index]`'s first byte. After a 416 resume to a
    // non-chunk-aligned offset, we slice the chunk from `offset -
    // chunk_start` so the Content-Range matches the bytes we actually send.
    let mut chunk_start: u64 = 0;

    while chunk_index < all_chunks.len() {
        let chunk = &all_chunks[chunk_index];
        // LINT: cast is safe — chunk lengths come from a Bytes buffer ≤ usize::MAX.
        #[allow(clippy::cast_possible_truncation)]
        let slice_start = (offset - chunk_start) as usize;
        let chunk_slice = chunk.slice(slice_start..);
        let chunk_len = chunk_slice.len() as u64;
        let end = offset + chunk_len - 1;
        let content_range = format!("bytes {offset}-{end}/{total_size}");

        let chunk_rid = new_request_id();
        let resp = http
            .put(&upload_url)
            .header("Content-Range", &content_range)
            .header("Content-Length", chunk_len.to_string())
            .header("client-request-id", &chunk_rid)
            .body(chunk_slice.to_vec())
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
            // RP2-F1: resume from the first expected range. The new offset
            // may not lie on a chunk boundary; `find_chunk_index` returns
            // both the containing chunk's index AND the start offset of
            // that chunk so we can slice the partial tail correctly.
            if let Some(range_str) = state.next_expected_ranges.first()
                && let Some(new_offset) = parse_range_start(range_str)
            {
                offset = new_offset;
                let (new_idx, new_start) = find_chunk_index(&all_chunks, new_offset);
                chunk_index = new_idx;
                chunk_start = new_start;
                continue;
            }
            return Err(GraphInternalError::InvalidRange {
                request_id: chunk_rid,
            });
        }

        if status == reqwest::StatusCode::ACCEPTED {
            // 202: server acknowledged the slice; advance past these bytes.
            offset += chunk_len;
            // RP2-F1: only move to the next chunk-index when we've consumed
            // the current chunk's full remaining tail. The partial-resume
            // case (slice_start > 0) typically lands offset on the next
            // chunk boundary, but we re-check explicitly to be safe.
            if offset >= chunk_start + chunk.len() as u64 {
                chunk_index += 1;
                chunk_start = offset;
            }
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

/// RP2-F1: returns `(chunk_index, chunk_start)` for the chunk whose
/// half-open byte range `[chunk_start, chunk_start + len)` contains
/// `target_offset`. When `target_offset` is past the last chunk's end, the
/// returned index is `chunks.len()` and `chunk_start` is the total byte
/// length — the caller's loop sees `chunk_index >= len` and exits.
///
/// The previous implementation returned the *next* chunk after the offset:
/// `target_offset = 3` against `chunks = [5, 5]` returned `1`, skipping
/// bytes 3–4 entirely and sending mis-aligned Content-Ranges to Graph.
fn find_chunk_index(chunks: &[bytes::Bytes], target_offset: u64) -> (usize, u64) {
    let mut cum_off = 0u64;
    for (i, chunk) in chunks.iter().enumerate() {
        let chunk_len = chunk.len() as u64;
        if target_offset < cum_off + chunk_len {
            return (i, cum_off);
        }
        cum_off += chunk_len;
    }
    (chunks.len(), cum_off)
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

    // RP2-F1: find_chunk_index unit tests pin the corrected math.

    #[test]
    fn rp2_f1_find_chunk_index_target_zero_returns_first_chunk() {
        let chunks = vec![Bytes::from_static(b"hello"), Bytes::from_static(b"world")];
        assert_eq!(find_chunk_index(&chunks, 0), (0, 0));
    }

    #[test]
    fn rp2_f1_find_chunk_index_within_first_chunk_returns_first_chunk() {
        let chunks = vec![Bytes::from_static(b"hello"), Bytes::from_static(b"world")];
        // Byte 3 lives in chunk 0 (range [0, 5)); pre-fix this returned 1.
        assert_eq!(find_chunk_index(&chunks, 3), (0, 0));
    }

    #[test]
    fn rp2_f1_find_chunk_index_at_boundary_returns_next_chunk_with_correct_start() {
        let chunks = vec![Bytes::from_static(b"hello"), Bytes::from_static(b"world")];
        assert_eq!(find_chunk_index(&chunks, 5), (1, 5));
    }

    #[test]
    fn rp2_f1_find_chunk_index_within_second_chunk_returns_second_chunk() {
        let chunks = vec![Bytes::from_static(b"hello"), Bytes::from_static(b"world")];
        assert_eq!(find_chunk_index(&chunks, 7), (1, 5));
    }

    #[test]
    fn rp2_f1_find_chunk_index_past_end_returns_len() {
        let chunks = vec![Bytes::from_static(b"hello"), Bytes::from_static(b"world")];
        assert_eq!(find_chunk_index(&chunks, 10), (2, 10));
        assert_eq!(find_chunk_index(&chunks, 999), (2, 10));
    }

    #[test]
    fn rp2_f1_find_chunk_index_empty_returns_zero() {
        let chunks: Vec<Bytes> = Vec::new();
        assert_eq!(find_chunk_index(&chunks, 0), (0, 0));
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
