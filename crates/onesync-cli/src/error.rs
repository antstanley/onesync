//! Top-level CLI error type. Maps RPC errors and local failures to the
//! stable exit-code table in `exit_codes`.

use onesync_protocol::errors::RpcError;
use onesync_protocol::rpc::JsonRpcError;

/// Errors returned by every CLI command.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Catch-all; exit code 1.
    #[error("{0}")]
    Generic(String),

    /// Argument validation failure; exit code 2. Clap mostly reports its own
    /// argument errors and exits 2 before we reach this branch, but the variant
    /// is reserved for future post-clap validations.
    #[error("invalid argument: {0}")]
    #[allow(dead_code)]
    InvalidArgs(String),

    /// The daemon socket is not reachable; exit code 3.
    #[error("daemon not running ({0})")]
    DaemonNotRunning(String),

    /// User needs to re-authenticate; exit code 4.
    #[error("authentication required")]
    AuthRequired,

    /// Pair is in Errored status; exit code 5.
    #[error("pair errored: {0}")]
    PairErrored(String),

    /// Conflict resolve failed because sides changed under us; exit code 6.
    #[error("conflict not resolved: {0}")]
    ConflictUnresolved(String),

    /// Local filesystem / permission error; exit code 7.
    #[error("permission: {0}")]
    Permission(String),

    /// Network or Graph API error; exit code 8.
    #[error("network: {0}")]
    Network(String),

    /// A spec'd limit was exceeded; exit code 9.
    #[error("limit exceeded: {0}")]
    LimitExceeded(String),

    /// CLI ≠ daemon major version; exit code 10.
    #[error("version mismatch with daemon: {0}")]
    VersionMajorMismatch(String),
}

impl From<JsonRpcError> for CliError {
    fn from(err: JsonRpcError) -> Self {
        // Decode embedded ErrorEnvelope (.data) when present; otherwise fall back to message.
        if let Some(envelope) = err
            .data
            .and_then(|v| serde_json::from_value::<onesync_protocol::errors::ErrorEnvelope>(v).ok())
        {
            return Self::from(RpcError {
                code: err.code,
                message: err.message,
                data: Some(envelope),
            });
        }
        Self::Generic(format!("{} (code {})", err.message, err.code))
    }
}

impl From<RpcError> for CliError {
    fn from(err: RpcError) -> Self {
        let kind = err.data.as_ref().map_or("", |d| d.kind.as_str());
        match kind {
            "auth.required" | "auth.token_revoked" => Self::AuthRequired,
            "pair.errored" => Self::PairErrored(err.message),
            "conflict.unresolved" => Self::ConflictUnresolved(err.message),
            "permission.denied" => Self::Permission(err.message),
            k if k.starts_with("network") || k.starts_with("graph") => Self::Network(err.message),
            k if k.starts_with("limit") => Self::LimitExceeded(err.message),
            "version.major_mismatch" => Self::VersionMajorMismatch(err.message),
            _ => Self::Generic(err.message),
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        if matches!(
            e.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
        ) {
            Self::DaemonNotRunning(e.to_string())
        } else {
            Self::Generic(e.to_string())
        }
    }
}

impl From<serde_json::Error> for CliError {
    fn from(e: serde_json::Error) -> Self {
        Self::Generic(format!("json: {e}"))
    }
}
