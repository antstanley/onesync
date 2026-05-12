//! Line-delimited JSON framing over a Unix stream socket.
//!
//! Each frame is a single UTF-8 JSON object followed by `\n`.
//! Frames larger than [`IPC_FRAME_MAX_BYTES`] are rejected with
//! [`FrameError::TooLarge`] without reading the remaining bytes.

use onesync_core::limits::IPC_FRAME_MAX_BYTES;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// Errors returned by [`read_frame`] and [`write_frame`].
#[derive(Debug, Error)]
pub enum FrameError {
    /// The incoming line exceeded [`IPC_FRAME_MAX_BYTES`].
    #[error("frame exceeds maximum size of {IPC_FRAME_MAX_BYTES} bytes")]
    TooLarge,
    /// The peer closed the connection.
    #[error("connection closed")]
    Closed,
    /// An underlying I/O error occurred.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

/// Read one newline-terminated JSON frame from `reader`.
///
/// # Errors
///
/// Returns [`FrameError::TooLarge`] if the line exceeds [`IPC_FRAME_MAX_BYTES`].
/// Returns [`FrameError::Closed`] if the peer closed the connection.
/// Returns [`FrameError::Io`] for other I/O failures.
pub async fn read_frame(reader: &mut BufReader<OwnedReadHalf>) -> Result<String, FrameError> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(FrameError::Closed);
    }
    // `n` is byte count including the `\n`.
    if n as u64 > IPC_FRAME_MAX_BYTES {
        return Err(FrameError::TooLarge);
    }
    // Strip trailing newline.
    if line.ends_with('\n') {
        line.pop();
        if line.ends_with('\r') {
            line.pop();
        }
    }
    Ok(line)
}

/// Write one JSON frame (appending `\n`) to `writer`.
///
/// # Errors
///
/// Returns [`FrameError::Io`] if the write fails.
pub async fn write_frame(writer: &mut OwnedWriteHalf, json: &str) -> Result<(), FrameError> {
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}
