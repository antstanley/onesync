//! Cloudflare-Tunnel webhook receiver.
//!
//! Per the 04-onedrive-adapter and 03-sync-engine decisions, Microsoft Graph delivers change
//! notifications via `POST /subscriptions` against a publicly-reachable callback URL. The
//! operator runs `cloudflared` against a config that the install docs ship; the tunnel
//! terminates on this daemon-hosted HTTP listener on
//! `InstanceConfig.webhook_listener_port`.
//!
//! Two request shapes are handled:
//!
//! 1. **Subscription validation** (`POST /callback?validationToken=…`): Graph sends an empty
//!    body and expects the validation token returned as `text/plain` within ~10 seconds. We
//!    echo the token back unchanged.
//!
//! 2. **Change notification** (`POST /callback` with a JSON body): we parse the array of
//!    notifications, look up the matching pair by `clientState`, and push a
//!    `Trigger::RemoteWebhook` into the scheduler.
//!
//! Polling via `/delta` remains the always-on fallback, so a flaky tunnel costs latency, not
//! correctness.
//!
//! **Carry-over for M10:** the Graph-side subscription registration (`POST /subscriptions`)
//! is not wired yet — `RemoteDrive::subscribe` / `unsubscribe` don't exist on the port. Once
//! the engine + adapter learn those calls, the scheduler will register subscriptions on
//! startup for every pair where `webhook_enabled = true`.

use std::sync::Arc;
use std::time::Duration;

use onesync_protocol::id::PairId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use onesync_core::ports::StateStore;

use crate::scheduler::SchedulerHandle;
use crate::shutdown::ShutdownToken;

/// Maximum request body length we'll read before giving up (defensive cap, ~256 KiB).
const MAX_BODY_BYTES: usize = 256 * 1024;

/// Read-deadline for one webhook delivery: Graph's notification window is ~30s.
const READ_TIMEOUT_S: u64 = 10;

/// Inputs the webhook receiver task needs.
pub struct ReceiverInputs {
    /// Local port to bind. `None` disables the receiver.
    pub port: Option<u16>,
    /// State store for the `clientState` → `pair_id` lookup.
    pub state: Arc<dyn StateStore>,
    /// Scheduler handle: the receiver pushes `Trigger::RemoteWebhook` through it.
    pub scheduler: SchedulerHandle,
}

/// Spawn the webhook receiver task. Returns `Ok(None)` if disabled.
///
/// # Errors
/// Returns an error if the configured port cannot be bound.
pub async fn spawn(
    inputs: ReceiverInputs,
    shutdown: &ShutdownToken,
) -> anyhow::Result<Option<u16>> {
    let Some(port) = inputs.port else {
        tracing::debug!("webhook receiver: port unset, receiver disabled");
        return Ok(None);
    };

    let addr = format!("127.0.0.1:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .map_err(|e| anyhow::anyhow!("webhook receiver: bind {addr} failed: {e}"))?;
    let bound_port = listener.local_addr()?.port();
    tracing::info!(port = bound_port, "webhook receiver: listening");

    let mut shutdown_rx = shutdown.subscribe();
    tokio::spawn(async move {
        let state = inputs.state;
        let scheduler = inputs.scheduler;
        loop {
            tokio::select! {
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, _addr)) => {
                            let state = state.clone();
                            let scheduler = scheduler.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, state, scheduler).await {
                                    tracing::warn!(error = %e, "webhook: per-conn handler errored");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "webhook: accept failed");
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    tracing::info!("webhook receiver: shutdown");
                    break;
                }
            }
        }
    });

    Ok(Some(bound_port))
}

async fn handle_connection(
    mut stream: TcpStream,
    state: Arc<dyn StateStore>,
    scheduler: SchedulerHandle,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 16 * 1024];
    let n = tokio::time::timeout(Duration::from_secs(READ_TIMEOUT_S), stream.read(&mut buf))
        .await
        .map_err(|_| anyhow::anyhow!("read timed out"))??;
    if n == 0 {
        return Err(anyhow::anyhow!("empty request"));
    }
    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
    let (request_line, headers_block, body_offset) = parse_request_head(&request)?;
    let (method, target) = split_request_line(request_line)?;
    if method != "POST" {
        write_simple_response(&mut stream, 405, "method not allowed").await?;
        return Ok(());
    }

    let body = body_from_buffer(&request, body_offset, headers_block)?;

    // Subscription validation path: ?validationToken=...
    if let Some(token) = extract_validation_token(target) {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {len}\r\n\r\n{token}",
            len = token.len(),
        );
        stream.write_all(response.as_bytes()).await?;
        return Ok(());
    }

    // Change notification path.
    let notifications: Notifications = serde_json::from_str(&body)
        .map_err(|e| anyhow::anyhow!("notification body parse failed: {e}"))?;
    for note in notifications.value {
        // M9 stores `clientState = pair_<ulid>`; map directly to a pair_id without state lookup.
        if let Some(pair_id) = parse_pair_id_from_client_state(&note.client_state) {
            // Confirm the pair exists + webhook-enabled before triggering.
            match state.pair_get(&pair_id).await {
                Ok(Some(pair)) if pair.webhook_enabled => {
                    let _ = scheduler.force_sync(pair_id).await;
                }
                Ok(_) => {
                    tracing::warn!(pair = %pair_id, "webhook: ignoring (pair missing or webhook_enabled=false)");
                }
                Err(e) => {
                    tracing::warn!(pair = %pair_id, error = %e, "webhook: pair_get failed");
                }
            }
        }
    }
    write_simple_response(&mut stream, 202, "accepted").await?;
    Ok(())
}

/// Notifications envelope per Graph webhook spec.
#[derive(Debug, serde::Deserialize)]
struct Notifications {
    value: Vec<Notification>,
}

#[derive(Debug, serde::Deserialize)]
struct Notification {
    #[serde(rename = "clientState", default)]
    client_state: String,
}

fn parse_request_head(req: &str) -> anyhow::Result<(&str, &str, usize)> {
    let split = req
        .find("\r\n\r\n")
        .ok_or_else(|| anyhow::anyhow!("malformed request (no header/body split)"))?;
    let head = &req[..split];
    let body_offset = split + 4;
    let (request_line, headers_block) = head
        .split_once("\r\n")
        .ok_or_else(|| anyhow::anyhow!("malformed request (no request line)"))?;
    Ok((request_line, headers_block, body_offset))
}

fn split_request_line(line: &str) -> anyhow::Result<(&str, &str)> {
    let mut parts = line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing method"))?;
    let target = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing target"))?;
    Ok((method, target))
}

fn body_from_buffer(req: &str, body_offset: usize, headers_block: &str) -> anyhow::Result<String> {
    let content_length = headers_block
        .lines()
        .find_map(|h| {
            let (k, v) = h.split_once(':')?;
            if k.eq_ignore_ascii_case("Content-Length") {
                v.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        anyhow::bail!("content-length {content_length} exceeds MAX_BODY_BYTES");
    }
    let body = &req[body_offset..];
    if body.len() < content_length {
        anyhow::bail!(
            "body length {} < Content-Length {content_length}",
            body.len()
        );
    }
    Ok(body[..content_length].to_owned())
}

fn extract_validation_token(target: &str) -> Option<&str> {
    let (_, query) = target.split_once('?')?;
    for kv in query.split('&') {
        if let Some(("validationToken", v)) = kv.split_once('=') {
            return Some(v);
        }
    }
    None
}

fn parse_pair_id_from_client_state(client_state: &str) -> Option<PairId> {
    if !client_state.starts_with("pair_") {
        return None;
    }
    client_state.parse().ok()
}

async fn write_simple_response(
    stream: &mut TcpStream,
    status: u16,
    body: &str,
) -> std::io::Result<()> {
    let reason = match status {
        202 => "Accepted",
        405 => "Method Not Allowed",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {len}\r\n\r\n{body}",
        len = body.len(),
    );
    stream.write_all(response.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_validation_token_from_query() {
        let token = extract_validation_token("/callback?validationToken=abc%20def");
        assert_eq!(token, Some("abc%20def"));
    }

    #[test]
    fn extract_validation_token_absent() {
        assert!(extract_validation_token("/callback").is_none());
        assert!(extract_validation_token("/callback?other=1").is_none());
    }

    #[test]
    fn pair_id_parsed_from_client_state() {
        let id = parse_pair_id_from_client_state("pair_01J8X7CFGMZG7Y4DC0VA8DZW2H");
        assert!(id.is_some());
    }

    #[test]
    fn pair_id_rejected_for_bogus_state() {
        assert!(parse_pair_id_from_client_state("not-a-pair").is_none());
        assert!(parse_pair_id_from_client_state("acct_01J8X7CFGMZG7Y4DC0VA8DZW2H").is_none());
    }

    #[test]
    fn parse_request_head_splits_header_from_body() {
        let req = "POST /callback HTTP/1.1\r\nContent-Length: 3\r\n\r\nabc";
        let (line, headers, body_offset) = parse_request_head(req).expect("parse");
        assert_eq!(line, "POST /callback HTTP/1.1");
        assert_eq!(headers, "Content-Length: 3");
        assert_eq!(&req[body_offset..], "abc");
    }
}
