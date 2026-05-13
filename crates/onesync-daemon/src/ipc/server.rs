//! Unix socket IPC server.
//!
//! Binds `<runtime_dir>/onesync.sock` with 0600 permissions, accepts
//! connections, and spawns one Tokio task per connection.  The server
//! task runs until the [`ShutdownToken`] fires, at which point it stops
//! accepting new connections and drops the listener (closing the socket).

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use anyhow::Context as _;
use onesync_protocol::rpc::JsonRpcNotification;
use tokio::net::UnixListener;
use tokio::sync::mpsc;

use crate::methods::{ConnCtx, DispatchCtx};
use crate::shutdown::ShutdownToken;

/// Bound depth of the per-connection notification channel feeding the outbound writer.
const CONN_NOTIF_DEPTH: usize = 512;

/// The filename of the IPC socket.
pub const SOCKET_FILE: &str = "onesync.sock";

/// Bind the Unix socket and run the accept loop until shutdown.
///
/// Each accepted connection is handed a clone of `ctx` and processed by
/// [`handle_connection`].  Removes any stale socket file before binding.
///
/// # Errors
///
/// Returns an error if the socket cannot be bound or its permissions
/// cannot be set.
pub async fn run(runtime_dir: &Path, token: ShutdownToken, ctx: DispatchCtx) -> anyhow::Result<()> {
    let sock_path = runtime_dir.join(SOCKET_FILE);

    // Remove stale socket from a previous crashed daemon.
    if sock_path.exists() {
        std::fs::remove_file(&sock_path)
            .with_context(|| format!("remove stale socket {}", sock_path.display()))?;
    }

    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("bind Unix socket {}", sock_path.display()))?;

    // Restrict socket to owner only (0600).
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set permissions on {}", sock_path.display()))?;

    tracing::info!(path = %sock_path.display(), "IPC socket listening");

    let mut shutdown_rx = token.subscribe();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        let ctx = ctx.clone();
                        tokio::spawn(handle_connection(stream, ctx));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept error on IPC socket");
                    }
                }
            }
            _ = shutdown_rx.recv() => {
                tracing::info!("IPC server shutting down");
                break;
            }
        }
    }

    // Clean up the socket file on graceful shutdown.
    let _ = std::fs::remove_file(&sock_path);

    Ok(())
}

/// Handle a single IPC connection.
///
/// Reads JSON-RPC frames, dispatches each through [`crate::ipc::dispatch`],
/// and writes the serialised response back. Concurrently, a writer task drains the
/// per-connection notification channel so subscription-emitting methods (`audit.tail`,
/// `pair.subscribe`, `conflict.subscribe`) can push `JsonRpcNotification` frames to the
/// same socket without competing with the request/response stream for the write half.
async fn handle_connection(stream: tokio::net::UnixStream, ctx: DispatchCtx) {
    use tokio::io::BufReader;

    use crate::ipc::framing::{FrameError, read_frame};

    let (read_half, write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    // Per-connection notification fan-in. Subscriptions on this connection clone the
    // sender (via ConnCtx.notif_tx) and push frames here.
    let (notif_tx, notif_rx) = mpsc::channel::<JsonRpcNotification>(CONN_NOTIF_DEPTH);

    // (response_tx, response_rx) carry serialised JSON-RPC responses from the dispatch
    // path to the writer task. Multiplexing responses + notifications onto one writer
    // half prevents interleaved partial frames.
    let (response_tx, response_rx) = mpsc::channel::<String>(CONN_NOTIF_DEPTH);

    let writer_handle = tokio::spawn(writer_task(write_half, notif_rx, response_rx));

    let conn = ConnCtx::new(ctx, notif_tx);

    loop {
        match read_frame(&mut reader).await {
            Ok(line) => {
                tracing::debug!(frame = %line, "ipc frame received");
                let response = parse_and_dispatch(&line, &conn).await;
                let serialised = serde_json::to_string(&response).unwrap_or_else(|_| {
                    r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"internal serialisation error"}}"#
                        .to_owned()
                });
                if response_tx.send(serialised).await.is_err() {
                    tracing::debug!("ipc writer task gone; closing connection");
                    break;
                }
            }
            Err(FrameError::Closed) => break,
            Err(FrameError::TooLarge) => {
                tracing::warn!("ipc frame too large; closing connection");
                break;
            }
            Err(FrameError::Io(e)) => {
                tracing::debug!(error = %e, "ipc read error");
                break;
            }
        }
    }

    drop(response_tx);
    drop(conn);
    let _ = writer_handle.await;
}

/// Multiplex outbound JSON-RPC responses and subscription notifications onto the
/// connection's write half. Runs until both channels close.
async fn writer_task(
    mut write_half: tokio::net::unix::OwnedWriteHalf,
    mut notif_rx: mpsc::Receiver<JsonRpcNotification>,
    mut response_rx: mpsc::Receiver<String>,
) {
    use crate::ipc::framing::write_frame;

    loop {
        tokio::select! {
            // tokio::select! randomises so notifications still drain even though both
            // request/response and subscription pumps share the write half.
            maybe_resp = response_rx.recv() => {
                if let Some(resp) = maybe_resp {
                    if let Err(e) = write_frame(&mut write_half, &resp).await {
                        tracing::debug!(error = %e, "ipc write error (response)");
                        return;
                    }
                } else {
                    // Request side closed; drain pending notifications then exit.
                    while let Some(notif) = notif_rx.recv().await {
                        let Ok(json) = serde_json::to_string(&notif) else { continue };
                        if write_frame(&mut write_half, &json).await.is_err() {
                            return;
                        }
                    }
                    return;
                }
            }
            maybe_notif = notif_rx.recv() => if let Some(notif) = maybe_notif {
                let Ok(json) = serde_json::to_string(&notif) else { continue };
                if let Err(e) = write_frame(&mut write_half, &json).await {
                    tracing::debug!(error = %e, "ipc write error (notification)");
                    return;
                }
            }
        }
    }
}

/// Parse a raw JSON line as a `JsonRpcRequest` and dispatch it.
///
/// Returns a parse-error response if the JSON is invalid.
async fn parse_and_dispatch(
    line: &str,
    ctx: &ConnCtx,
) -> onesync_protocol::rpc::JsonRpcResponse {
    use onesync_protocol::rpc::{self, JsonRpcResponse};

    match serde_json::from_str::<onesync_protocol::rpc::JsonRpcRequest>(line) {
        Ok(req) => crate::ipc::dispatch::dispatch(&req, ctx).await,
        Err(e) => JsonRpcResponse::error(
            None::<String>,
            rpc::PARSE_ERROR,
            format!("parse error: {e}"),
        ),
    }
}
