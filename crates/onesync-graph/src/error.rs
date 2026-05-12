//! Internal error type and mapping to the port-level [`GraphError`].

use onesync_core::ports::GraphError;

/// Internal rich error type carrying HTTP status, Microsoft error codes, request-ids, etc.
///
/// Only the adapter layer sees this type; callers outside the crate receive [`GraphError`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GraphInternalError {
    /// 401 after one refresh attempt.
    #[error("unauthorized (request-id: {request_id})")]
    Unauthorized {
        /// Graph `request-id` header for traceability.
        request_id: String,
    },
    /// 401 `invalid_grant` on the refresh-token exchange; user must re-authenticate.
    #[error("re-auth required (request-id: {request_id})")]
    ReAuthRequired {
        /// Graph `request-id` header for traceability.
        request_id: String,
    },
    /// 403.
    #[error("forbidden (request-id: {request_id})")]
    Forbidden {
        /// Graph `request-id` header for traceability.
        request_id: String,
    },
    /// 404.
    #[error("not found (request-id: {request_id})")]
    NotFound {
        /// Graph `request-id` header for traceability.
        request_id: String,
    },
    /// 409 `nameAlreadyExists`.
    #[error("name conflict (request-id: {request_id})")]
    NameConflict {
        /// Graph `request-id` header for traceability.
        request_id: String,
    },
    /// 410 `resyncRequired`.
    #[error("resync required (request-id: {request_id})")]
    ResyncRequired {
        /// Graph `request-id` header for traceability.
        request_id: String,
    },
    /// 412 `preconditionFailed`; carries the server `ETag`.
    #[error("stale (server_etag: {server_etag}, request-id: {request_id})")]
    Stale {
        /// The `ETag` the server currently holds.
        server_etag: String,
        /// Graph `request-id` header for traceability.
        request_id: String,
    },
    /// 416 invalid range (upload session resume).
    #[error("invalid range (request-id: {request_id})")]
    InvalidRange {
        /// Graph `request-id` header for traceability.
        request_id: String,
    },
    /// 429 or 503 with `Retry-After`.
    #[error("throttled {retry_after_s}s (request-id: {request_id})")]
    Throttled {
        /// Seconds to wait per `Retry-After`.
        retry_after_s: u64,
        /// Graph `request-id` header for traceability.
        request_id: String,
    },
    /// 5xx without `Retry-After`.
    #[error("transient: {detail} (request-id: {request_id})")]
    Transient {
        /// Human-readable detail.
        detail: String,
        /// Graph `request-id` header for traceability.
        request_id: String,
    },
    /// Network/DNS/TLS failure.
    #[error("network: {detail}")]
    Network {
        /// Human-readable detail from the underlying error.
        detail: String,
    },
    /// Response body did not match the expected shape.
    #[error("decode: {detail}")]
    Decode {
        /// Description of the decoding failure.
        detail: String,
    },
    /// Downloaded content hash did not match the server-supplied hash.
    #[error("hash mismatch")]
    HashMismatch,
    /// File exceeds `MAX_FILE_SIZE_BYTES`.
    #[error("file too large")]
    TooLarge,
    /// Listener timed out waiting for the OAuth redirect.
    #[error("auth listener timeout")]
    Timeout,
    /// An argument was invalid (e.g., payload too large for small-upload helper).
    #[error("invalid argument: {detail}")]
    InvalidArgument {
        /// Description of the argument problem.
        detail: String,
    },
}

/// Map an internal error to the port-level [`GraphError`].
///
/// The internal error is logged (request-id etc.) but not surfaced to the engine.
#[must_use]
pub fn map_to_port(err: GraphInternalError) -> GraphError {
    match err {
        GraphInternalError::Unauthorized { .. } => GraphError::Unauthorized,
        GraphInternalError::ReAuthRequired { .. } => GraphError::ReAuthRequired,
        GraphInternalError::Forbidden { .. } => GraphError::Forbidden,
        GraphInternalError::NotFound { .. } => GraphError::NotFound,
        GraphInternalError::NameConflict { .. } => GraphError::NameConflict,
        GraphInternalError::ResyncRequired { .. } => GraphError::ResyncRequired,
        GraphInternalError::Stale { server_etag, .. } => GraphError::Stale { server_etag },
        GraphInternalError::InvalidRange { .. } => GraphError::InvalidRange,
        GraphInternalError::Throttled { retry_after_s, .. } => {
            GraphError::Throttled { retry_after_s }
        }
        GraphInternalError::Transient { detail, .. } => GraphError::Transient(detail),
        GraphInternalError::Network { detail } => GraphError::Network { detail },
        GraphInternalError::Decode { detail } => GraphError::Decode { detail },
        GraphInternalError::HashMismatch => GraphError::HashMismatch,
        GraphInternalError::TooLarge => GraphError::TooLarge,
        GraphInternalError::Timeout => GraphError::Transient("auth listener timeout".to_owned()),
        GraphInternalError::InvalidArgument { detail } => {
            GraphError::Transient(format!("invalid argument: {detail}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> String {
        "req-123".to_owned()
    }

    #[test]
    fn unauthorized_maps() {
        assert!(matches!(
            map_to_port(GraphInternalError::Unauthorized { request_id: req() }),
            GraphError::Unauthorized
        ));
    }

    #[test]
    fn re_auth_maps() {
        assert!(matches!(
            map_to_port(GraphInternalError::ReAuthRequired { request_id: req() }),
            GraphError::ReAuthRequired
        ));
    }

    #[test]
    fn throttled_maps_retry_after() {
        let ge = map_to_port(GraphInternalError::Throttled {
            retry_after_s: 42,
            request_id: req(),
        });
        assert!(matches!(ge, GraphError::Throttled { retry_after_s: 42 }));
    }

    #[test]
    fn stale_carries_etag() {
        let ge = map_to_port(GraphInternalError::Stale {
            server_etag: "etag-abc".to_owned(),
            request_id: req(),
        });
        assert!(matches!(ge, GraphError::Stale { server_etag } if server_etag == "etag-abc"));
    }

    #[test]
    fn hash_mismatch_maps() {
        assert!(matches!(
            map_to_port(GraphInternalError::HashMismatch),
            GraphError::HashMismatch
        ));
    }

    #[test]
    fn too_large_maps() {
        assert!(matches!(
            map_to_port(GraphInternalError::TooLarge),
            GraphError::TooLarge
        ));
    }

    #[test]
    fn network_maps() {
        let ge = map_to_port(GraphInternalError::Network {
            detail: "connection refused".to_owned(),
        });
        assert!(matches!(ge, GraphError::Network { .. }));
    }

    #[test]
    fn decode_maps() {
        let ge = map_to_port(GraphInternalError::Decode {
            detail: "missing field".to_owned(),
        });
        assert!(matches!(ge, GraphError::Decode { .. }));
    }

    #[test]
    fn resync_required_maps() {
        assert!(matches!(
            map_to_port(GraphInternalError::ResyncRequired { request_id: req() }),
            GraphError::ResyncRequired
        ));
    }

    #[test]
    fn forbidden_maps() {
        assert!(matches!(
            map_to_port(GraphInternalError::Forbidden { request_id: req() }),
            GraphError::Forbidden
        ));
    }

    #[test]
    fn not_found_maps() {
        assert!(matches!(
            map_to_port(GraphInternalError::NotFound { request_id: req() }),
            GraphError::NotFound
        ));
    }

    #[test]
    fn name_conflict_maps() {
        assert!(matches!(
            map_to_port(GraphInternalError::NameConflict { request_id: req() }),
            GraphError::NameConflict
        ));
    }

    #[test]
    fn invalid_range_maps() {
        assert!(matches!(
            map_to_port(GraphInternalError::InvalidRange { request_id: req() }),
            GraphError::InvalidRange
        ));
    }

    #[test]
    fn timeout_maps_to_transient() {
        assert!(matches!(
            map_to_port(GraphInternalError::Timeout),
            GraphError::Transient(_)
        ));
    }

    #[test]
    fn invalid_argument_maps_to_transient() {
        assert!(matches!(
            map_to_port(GraphInternalError::InvalidArgument {
                detail: "too big".to_owned()
            }),
            GraphError::Transient(_)
        ));
    }
}
