//! One-shot loopback HTTP listener for the OAuth redirect.
//!
//! Binds on `127.0.0.1:0` (ephemeral port), returns `(listener, port)`.
//! [`await_code`] accepts exactly one inbound HTTP request, parses
//! `?code=…&state=…`, verifies `state`, and returns the code.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::error::GraphInternalError;

/// Bind a loopback listener on an ephemeral port.
///
/// # Errors
///
/// Returns [`GraphInternalError::Network`] if the OS cannot allocate a port.
pub async fn bind() -> Result<(TcpListener, u16), GraphInternalError> {
    let listener =
        TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| GraphInternalError::Network {
                detail: format!("failed to bind loopback listener: {e}"),
            })?;
    let port = listener
        .local_addr()
        .map_err(|e| GraphInternalError::Network {
            detail: format!("failed to get listener port: {e}"),
        })?
        .port();
    Ok((listener, port))
}

/// Accept one connection on `listener`, parse `?code=…&state=…` from the request line,
/// verify `state == expected_state`, send a 200 HTTP response, and return `(code, state)`.
///
/// Times out after `timeout_s` seconds.
///
/// # Errors
///
/// - [`GraphInternalError::Timeout`] if no connection arrives before `timeout_s` elapses.
/// - [`GraphInternalError::Decode`] if the HTTP request line is malformed or `state` mismatches.
/// - [`GraphInternalError::Network`] on I/O errors.
pub async fn await_code(
    listener: TcpListener,
    expected_state: &str,
    timeout_s: u64,
) -> Result<(String, String), GraphInternalError> {
    let accept_fut = async {
        let (mut stream, _addr) =
            listener
                .accept()
                .await
                .map_err(|e| GraphInternalError::Network {
                    detail: format!("listener accept failed: {e}"),
                })?;

        let mut buf = vec![0u8; 4096];
        let n = stream
            .read(&mut buf)
            .await
            .map_err(|e| GraphInternalError::Network {
                detail: format!("read from accepted stream failed: {e}"),
            })?;
        let request = String::from_utf8_lossy(&buf[..n]);

        // Parse the request line: "GET /callback?code=X&state=Y HTTP/1.1"
        let (code, state) = parse_code_and_state(&request)?;

        if state != expected_state {
            return Err(GraphInternalError::Decode {
                detail: format!("OAuth state mismatch: expected {expected_state:?}, got {state:?}"),
            });
        }

        // Send a minimal HTTP 200 response.
        let response =
            "HTTP/1.1 200 OK\r\nContent-Length: 36\r\n\r\nAuthentication complete. Close this tab.";
        stream
            .write_all(response.as_bytes())
            .await
            .map_err(|e| GraphInternalError::Network {
                detail: format!("write response failed: {e}"),
            })?;

        Ok::<_, GraphInternalError>((code, state))
    };

    tokio::time::timeout(std::time::Duration::from_secs(timeout_s), accept_fut)
        .await
        .map_err(|_| GraphInternalError::Timeout)?
}

/// Parse `code` and `state` query parameters from a raw HTTP request.
fn parse_code_and_state(request: &str) -> Result<(String, String), GraphInternalError> {
    // First line: "GET /callback?code=X&state=Y HTTP/1.1"
    let first_line = request
        .lines()
        .next()
        .ok_or_else(|| GraphInternalError::Decode {
            detail: "empty HTTP request".to_owned(),
        })?;

    // Extract the path+query portion.
    let path_part =
        first_line
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| GraphInternalError::Decode {
                detail: "malformed HTTP request line".to_owned(),
            })?;

    // Parse query string.
    let query = path_part.split_once('?').map_or("", |(_, q)| q);

    let mut code: Option<String> = None;
    let mut state: Option<String> = None;

    for kv in query.split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            match k {
                "code" => code = Some(url_decode(v)),
                "state" => state = Some(url_decode(v)),
                _ => {}
            }
        }
    }

    let code = code.ok_or_else(|| GraphInternalError::Decode {
        detail: "OAuth redirect missing 'code' parameter".to_owned(),
    })?;
    let state = state.ok_or_else(|| GraphInternalError::Decode {
        detail: "OAuth redirect missing 'state' parameter".to_owned(),
    })?;

    Ok((code, state))
}

/// Minimal percent-decode (replaces `%XX` sequences; does not handle `+`).
fn url_decode(s: &str) -> String {
    // For our purposes the code and state don't normally contain percent-encoded chars,
    // but we handle the simple case for correctness.
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_digit(bytes[i + 1]), hex_digit(bytes[i + 2]))
        {
            result.push(char::from((hi << 4) | lo));
            i += 3;
            continue;
        }
        result.push(char::from(bytes[i]));
        i += 1;
    }
    result
}

const fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn happy_path_returns_code_and_state() {
        let (listener, port) = bind().await.unwrap();
        let expected_state = "test-state-abc";

        let handle = tokio::spawn(async move { await_code(listener, expected_state, 5).await });

        // Give the listener task a moment to start accepting.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Send a hand-crafted HTTP request.
        let mut conn = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let req = format!(
            "GET /callback?code=my-auth-code&state={expected_state} HTTP/1.1\r\nHost: localhost\r\n\r\n"
        );
        conn.write_all(req.as_bytes()).await.unwrap();
        drop(conn);

        let (code, state) = handle.await.unwrap().unwrap();
        assert_eq!(code, "my-auth-code");
        assert_eq!(state, expected_state);
    }

    #[tokio::test]
    async fn state_mismatch_returns_decode_error() {
        let (listener, port) = bind().await.unwrap();

        let handle = tokio::spawn(async move { await_code(listener, "expected-state", 5).await });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut conn = tokio::net::TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        let req = "GET /callback?code=code&state=WRONG HTTP/1.1\r\nHost: localhost\r\n\r\n";
        conn.write_all(req.as_bytes()).await.unwrap();
        drop(conn);

        let err = handle.await.unwrap().unwrap_err();
        assert!(
            matches!(err, GraphInternalError::Decode { .. }),
            "expected Decode error, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn timeout_returns_timeout_error() {
        let (listener, _port) = bind().await.unwrap();
        // Use 1-second timeout and don't send anything.
        let result = await_code(listener, "state", 1).await;
        assert!(
            matches!(result, Err(GraphInternalError::Timeout)),
            "expected Timeout, got: {result:?}"
        );
    }
}
