//! JSON-RPC 2.0 wire types for the IPC channel between `onesyncd` and the CLI.
//!
//! The daemon speaks line-delimited JSON over a Unix socket. Each line is one
//! of the types below. Error codes follow the JSON-RPC 2.0 spec:
//! <https://www.jsonrpc.org/specification#error_object>

use serde::{Deserialize, Serialize};

// ── Standard JSON-RPC 2.0 error codes ───────────────────────────────────────

/// Parse error: invalid JSON received.
pub const PARSE_ERROR: i32 = -32_700;
/// Invalid request: the JSON was not a valid Request object.
pub const INVALID_REQUEST: i32 = -32_600;
/// Method not found: the requested method does not exist or is not available.
pub const METHOD_NOT_FOUND: i32 = -32_601;
/// Invalid params: invalid method parameters.
pub const INVALID_PARAMS: i32 = -32_602;
/// Internal error: internal JSON-RPC error.
pub const INTERNAL_ERROR: i32 = -32_603;
/// Application error base: codes from this value downward are application-defined.
pub const APP_ERROR_BASE: i32 = -32_000;

// ── Wire types ───────────────────────────────────────────────────────────────

/// A JSON-RPC 2.0 request object.
///
/// `id` is `None` for notifications sent by the client (unusual but spec-valid);
/// for daemon use, id is always present.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// Protocol version string, always `"2.0"`.
    pub jsonrpc: String,
    /// Request identifier. `None` for notifications.
    pub id: Option<serde_json::Value>,
    /// Name of the method to invoke.
    pub method: String,
    /// Method parameters (object or array per spec).
    #[serde(default)]
    pub params: serde_json::Value,
}

impl JsonRpcRequest {
    /// Construct a request with the given `id`, `method`, and `params`.
    #[must_use]
    pub fn new(
        id: impl Into<serde_json::Value>,
        method: impl Into<String>,
        params: serde_json::Value,
    ) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id: Some(id.into()),
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 response (either success or error).
///
/// The `#[serde(untagged)]` attribute means the JSON shape itself distinguishes
/// the two variants: a success carries `"result"` and an error carries `"error"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcResponse {
    /// Successful response.
    Ok(JsonRpcOk),
    /// Error response.
    Err(JsonRpcErrorResponse),
}

impl JsonRpcResponse {
    /// Build a successful response.
    #[must_use]
    pub fn ok(id: impl Into<String>, result: serde_json::Value) -> Self {
        Self::Ok(JsonRpcOk {
            jsonrpc: "2.0".to_owned(),
            id: id.into(),
            result,
        })
    }

    /// Build an error response with an optional `id`.
    #[must_use]
    pub fn error(id: Option<impl Into<String>>, code: i32, message: impl Into<String>) -> Self {
        Self::Err(JsonRpcErrorResponse {
            jsonrpc: "2.0".to_owned(),
            id: id.map(Into::into),
            error: JsonRpcError {
                code,
                message: message.into(),
                data: None,
            },
        })
    }
}

/// The success variant of a JSON-RPC response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcOk {
    /// Protocol version, always `"2.0"`.
    pub jsonrpc: String,
    /// Echoed request id.
    pub id: String,
    /// Return value of the method.
    pub result: serde_json::Value,
}

/// The error variant of a JSON-RPC response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcErrorResponse {
    /// Protocol version, always `"2.0"`.
    pub jsonrpc: String,
    /// Echoed request id; `None` if the request id could not be determined.
    pub id: Option<String>,
    /// Error payload.
    pub error: JsonRpcError,
}

/// Error payload carried in a JSON-RPC error response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Numeric error code (see the `*_ERROR` / `APP_ERROR_BASE` constants).
    pub code: i32,
    /// Human-readable error message.
    pub message: String,
    /// Optional structured error data (e.g. validation details).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// A JSON-RPC 2.0 notification (a request without an `id`, sent by the daemon
/// to push events to subscribers).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    /// Protocol version, always `"2.0"`.
    pub jsonrpc: String,
    /// Notification method name (e.g. `"onesync.progress"`).
    pub method: String,
    /// Notification parameters.
    pub params: serde_json::Value,
}

impl JsonRpcNotification {
    /// Construct a notification with the given `method` and `params`.
    #[must_use]
    pub fn new(method: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            method: method.into(),
            params,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_through_json() {
        let req = JsonRpcRequest::new("req-1", "health.ping", serde_json::Value::Null);
        let json = serde_json::to_string(&req).unwrap();
        let back: JsonRpcRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.method, "health.ping");
        assert_eq!(back.jsonrpc, "2.0");
    }

    #[test]
    fn ok_response_round_trips() {
        let resp = JsonRpcResponse::ok("req-1", serde_json::json!({"status": "ok"}));
        let json = serde_json::to_string(&resp).unwrap();
        let back: JsonRpcResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, JsonRpcResponse::Ok(_)));
    }

    #[test]
    fn error_response_round_trips() {
        let resp: Option<String> = None;
        let resp = JsonRpcResponse::error(resp, INTERNAL_ERROR, "something went wrong");
        let json = serde_json::to_string(&resp).unwrap();
        let back: JsonRpcResponse = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, JsonRpcResponse::Err(_)));
    }

    #[test]
    fn notification_round_trips() {
        let notif = JsonRpcNotification::new("onesync.progress", serde_json::json!({"pct": 50}));
        let json = serde_json::to_string(&notif).unwrap();
        let back: JsonRpcNotification = serde_json::from_str(&json).unwrap();
        assert_eq!(back.method, "onesync.progress");
        assert_eq!(back.params["pct"], 50);
    }

    #[test]
    fn error_codes_have_expected_values() {
        assert_eq!(PARSE_ERROR, -32_700);
        assert_eq!(INVALID_REQUEST, -32_600);
        assert_eq!(METHOD_NOT_FOUND, -32_601);
        assert_eq!(INVALID_PARAMS, -32_602);
        assert_eq!(INTERNAL_ERROR, -32_603);
        assert_eq!(APP_ERROR_BASE, -32_000);
    }
}
