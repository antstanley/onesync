//! Streaming download with SHA-1 and `QuickXorHash` verification.

use bytes::Bytes;
use futures::TryStreamExt;
use onesync_protocol::primitives::DriveId;
use sha1::Sha1;
use sha2::Digest;

use crate::error::GraphInternalError;
use crate::items::FileHashes;

const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";

/// Download a drive item by id, verify its hash, and return the bytes.
///
/// - Follows the 302 redirect to the pre-signed storage URL (no auth header on the
///   second request).
/// - Streams the body in [`onesync_core::limits::HASH_BLOCK_BYTES`]-sized chunks.
/// - Verifies `sha1Hash` (Personal) or `quickXorHash` (Business) when present in
///   `expected`; returns [`GraphInternalError::HashMismatch`] on mismatch.
///
/// # Errors
///
/// Returns [`GraphInternalError`] on network, decode, or hash-mismatch failures.
pub async fn download(
    http: &reqwest::Client,
    token: &str,
    drive_id: &DriveId,
    item_id: &str,
    expected: Option<&FileHashes>,
) -> Result<Bytes, GraphInternalError> {
    let url = format!(
        "{GRAPH_BASE}/drives/{}/items/{item_id}/content",
        drive_id.as_str()
    );
    download_from_url(http, token, &url, expected).await
}

/// Download from an explicit URL (allows tests to inject wiremock URLs).
pub async fn download_from_url(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    expected: Option<&FileHashes>,
) -> Result<Bytes, GraphInternalError> {
    use onesync_core::limits::HASH_BLOCK_BYTES;

    let request_id = new_request_id();
    // First request — follow redirects automatically (reqwest default).
    let resp = http
        .get(url)
        .bearer_auth(token)
        .header("client-request-id", &request_id)
        .send()
        .await
        .map_err(|e| GraphInternalError::Network {
            detail: e.to_string(),
        })?;

    let checked = crate::client::check_status(resp, &request_id).await?;

    // Stream and accumulate with hash computation.
    let mut sha1_hasher = Sha1::new();
    let mut qxh = QuickXorHasher::new();
    let mut all_bytes: Vec<u8> = Vec::new();

    let mut stream = checked.bytes_stream();
    // Collect chunks
    let mut chunk_buf: Vec<u8> = Vec::with_capacity(HASH_BLOCK_BYTES);

    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|e| GraphInternalError::Network {
            detail: e.to_string(),
        })?
    {
        chunk_buf.extend_from_slice(&chunk);
        // Process full blocks.
        while chunk_buf.len() >= HASH_BLOCK_BYTES {
            let block = chunk_buf.drain(..HASH_BLOCK_BYTES).collect::<Vec<_>>();
            sha1_hasher.update(&block);
            qxh.update(&block);
            all_bytes.extend_from_slice(&block);
        }
    }
    // Process remainder.
    if !chunk_buf.is_empty() {
        sha1_hasher.update(&chunk_buf);
        qxh.update(&chunk_buf);
        all_bytes.extend_from_slice(&chunk_buf);
    }

    // Verify hashes if expected.
    if let Some(hashes) = expected {
        if let Some(expected_sha1) = &hashes.sha1_hash {
            let computed = hex_encode(sha1_hasher.finalize().as_slice());
            if !computed.eq_ignore_ascii_case(expected_sha1) {
                return Err(GraphInternalError::HashMismatch);
            }
        }
        if let Some(expected_qxh) = &hashes.quick_xor_hash {
            let computed = base64_encode_qxh(qxh.finalize());
            if computed != *expected_qxh {
                return Err(GraphInternalError::HashMismatch);
            }
        }
    }

    Ok(Bytes::from(all_bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn base64_encode_qxh(bytes: [u8; 20]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
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

// ── QuickXorHash ──────────────────────────────────────────────────────────────
//
// Microsoft's rolling XOR-shift hash, defined at:
// https://learn.microsoft.com/en-us/onedrive/developer/code-snippets/quickxorhash
//
// Algorithm:
// - 20 bytes (160 bits) of state.
// - Each byte of input is XOR'd into the state at a bit position that advances by 11 each byte.
// - Bit position wraps modulo 160 (20 × 8).
// - Final state is returned as a 20-byte array.

/// Stateful `QuickXorHash` accumulator.
pub struct QuickXorHasher {
    data: [u8; 20],
    length_so_far: u64,
    shift: usize,
}

impl QuickXorHasher {
    const WIDTHBITS: usize = 160;
    const BIT_SHIFT: usize = 11;

    /// Create a new zeroed hasher.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            data: [0u8; 20],
            length_so_far: 0,
            shift: 0,
        }
    }

    /// Feed bytes into the hasher.
    pub fn update(&mut self, input: &[u8]) {
        for &b in input {
            // Determine which byte and bit offset within `data` to XOR into.
            let bit_index = self.shift % Self::WIDTHBITS;
            let byte_index = bit_index / 8;
            let bit_in_byte = bit_index % 8;

            // Spread the byte across two consecutive state bytes (wrapping).
            self.data[byte_index] ^= b << bit_in_byte;
            if bit_in_byte > 0 {
                let next_byte = (byte_index + 1) % 20;
                self.data[next_byte] ^= b >> (8 - bit_in_byte);
            }

            self.shift = (self.shift + Self::BIT_SHIFT) % Self::WIDTHBITS;
        }
        self.length_so_far += input.len() as u64;
    }

    /// Finalise the hash: XOR the length into the last 8 bytes and return the 20-byte digest.
    #[must_use]
    pub fn finalize(mut self) -> [u8; 20] {
        // XOR length into the last 8 bytes (little-endian).
        let len_bytes = self.length_so_far.to_le_bytes();
        for (i, lb) in len_bytes.iter().enumerate() {
            self.data[12 + i] ^= lb;
        }
        self.data
    }
}

impl Default for QuickXorHasher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::Digest;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client() -> reqwest::Client {
        reqwest::Client::builder().use_rustls_tls().build().unwrap()
    }

    fn sha1_hex(data: &[u8]) -> String {
        let mut h = Sha1::new();
        h.update(data);
        hex_encode(h.finalize().as_slice())
    }

    #[tokio::test]
    async fn download_happy_path_sha1_matches() {
        let server = MockServer::start().await;
        let content = b"Hello, OneDrive!";
        let expected_sha1 = sha1_hex(content);

        Mock::given(method("GET"))
            .and(path("/file"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(content.as_slice()))
            .mount(&server)
            .await;

        let http = make_client();
        let url = format!("{}/file", server.uri());
        let hashes = FileHashes {
            sha1_hash: Some(expected_sha1.to_uppercase()),
            quick_xor_hash: None,
        };
        let bytes = download_from_url(&http, "tok", &url, Some(&hashes))
            .await
            .unwrap();
        assert_eq!(&bytes[..], content.as_slice());
    }

    #[tokio::test]
    async fn download_hash_mismatch_returns_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/file"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"Wrong content".as_slice()))
            .mount(&server)
            .await;

        let http = make_client();
        let url = format!("{}/file", server.uri());
        let hashes = FileHashes {
            sha1_hash: Some("da39a3ee5e6b4b0d3255bfef95601890afd80709".to_owned()), // SHA1 of empty
            quick_xor_hash: None,
        };
        let err = download_from_url(&http, "tok", &url, Some(&hashes))
            .await
            .unwrap_err();
        assert!(
            matches!(err, GraphInternalError::HashMismatch),
            "expected HashMismatch, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn download_no_hashes_returns_bytes() {
        let server = MockServer::start().await;
        let content = b"raw data";
        Mock::given(method("GET"))
            .and(path("/file"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(content.as_slice()))
            .mount(&server)
            .await;

        let http = make_client();
        let url = format!("{}/file", server.uri());
        let bytes = download_from_url(&http, "tok", &url, None).await.unwrap();
        assert_eq!(&bytes[..], content.as_slice());
    }

    #[test]
    fn quickxorhash_empty_is_all_zeros_except_length() {
        let h = QuickXorHasher::new();
        let result = h.finalize();
        // Length is 0; all state stays at zero except the length XOR, but length=0 → no change.
        assert_eq!(result, [0u8; 20]);
    }

    #[test]
    fn quickxorhash_deterministic() {
        let mut h1 = QuickXorHasher::new();
        h1.update(b"hello world");
        let mut h2 = QuickXorHasher::new();
        h2.update(b"hello world");
        assert_eq!(h1.finalize(), h2.finalize());
    }

    #[test]
    fn quickxorhash_differs_for_different_input() {
        let mut h1 = QuickXorHasher::new();
        h1.update(b"foo");
        let mut h2 = QuickXorHasher::new();
        h2.update(b"bar");
        assert_ne!(h1.finalize(), h2.finalize());
    }
}
