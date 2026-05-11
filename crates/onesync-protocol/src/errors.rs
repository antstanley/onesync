//! Structured error envelope used by ports, persisted ops, and the IPC.

use serde::{Deserialize, Serialize};

/// Structured error payload attached to failed operations and IPC responses.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    /// Machine-readable error kind (e.g. `"not_found"`, `"quota_exceeded"`).
    pub kind: String,
    /// Human-readable description of the error.
    pub message: String,
    /// Whether the caller may safely retry this operation.
    pub retryable: bool,
    /// Correlation id of the originating request, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Arbitrary key-value pairs providing additional diagnostic context.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub context: serde_json::Map<String, serde_json::Value>,
}

/// JSON-RPC 2.0–style error object returned by the IPC layer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    /// Numeric error code (use JSON-RPC reserved range or application codes).
    pub code: i32,
    /// Short human-readable description of the error.
    pub message: String,
    /// Optional structured payload with additional detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ErrorEnvelope>,
}
