//! Minimal JSON-RPC 2.0 client over the daemon's Unix socket.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, BufWriter};
use tokio::net::UnixStream;

use onesync_protocol::rpc::{JsonRpcRequest, JsonRpcResponse};

use crate::error::CliError;

// Cast a JSON id (we issue strings via `id_<ulid>`) into the typed Value expected
// by the daemon's JsonRpcRequest::id field.
const fn id_value(s: String) -> serde_json::Value {
    serde_json::Value::String(s)
}

/// Default socket path: `${TMPDIR}onesync/onesync.sock`.
///
/// `TMPDIR` is read once at process start. The disallowed-methods lint flags
/// `std::env::var` because the engine should route through `InstanceConfig`;
/// here the CLI is asking the harness for the runtime-dir before any RPC, so
/// reading the env var directly is the right surface.
#[allow(clippy::disallowed_methods, clippy::missing_const_for_fn)]
// LINT: CLI startup runtime-dir resolution; not engine code. Not `const` because
//       `std::env::var` and `PathBuf::push` are non-const.
pub fn default_socket_path() -> PathBuf {
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp/".into());
    let mut p = PathBuf::from(tmpdir);
    p.push("onesync");
    p.push("onesync.sock");
    p
}

/// A connection to the daemon over a Unix socket.
pub struct RpcClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: BufWriter<tokio::net::unix::OwnedWriteHalf>,
    next_id: AtomicU64,
}

impl RpcClient {
    /// Open a fresh connection.
    pub async fn connect(socket: &Path) -> Result<Self, CliError> {
        let stream = UnixStream::connect(socket).await?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer: BufWriter::new(writer),
            next_id: AtomicU64::new(1),
        })
    }

    /// Send a one-shot request and decode the response's `result` as `R`.
    pub async fn call<R: DeserializeOwned>(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<R, CliError> {
        let id = format!("req_{:020}", self.next_id.fetch_add(1, Ordering::Relaxed));
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_owned(),
            id: Some(id_value(id)),
            method: method.to_owned(),
            params,
        };
        let bytes = serde_json::to_vec(&req)?;
        self.writer.write_all(&bytes).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;

        let mut line = String::new();
        let n = self.reader.read_line(&mut line).await?;
        if n == 0 {
            return Err(CliError::Generic("daemon closed the connection".into()));
        }
        let resp: JsonRpcResponse = serde_json::from_str(line.trim_end())?;
        match resp {
            JsonRpcResponse::Ok(ok) => Ok(serde_json::from_value(ok.result)?),
            JsonRpcResponse::Err(err) => Err(CliError::from(err.error)),
        }
    }
}
