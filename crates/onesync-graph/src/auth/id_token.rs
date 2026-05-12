//! Parse the JWT `id_token` payload to extract `tid`, `oid`, `upn`, and `display_name`.
//!
//! No signature verification is performed: the token was received over a TLS-authenticated
//! HTTPS connection from the Microsoft identity platform, so we trust the channel.

use base64::Engine as _;
use onesync_protocol::enums::AccountKind;

/// Microsoft's tenant ID for consumer MSA (personal) accounts.
pub const CONSUMER_TENANT_ID: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

/// Claims extracted from the `id_token`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdTokenClaims {
    /// Azure AD tenant identifier.
    pub tid: String,
    /// Object (user) identifier within the tenant.
    pub oid: String,
    /// User Principal Name / email.
    pub upn: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Derived account kind based on `tid`.
    pub kind: AccountKind,
}

/// Decode the `id_token` JWT payload and extract claims.
///
/// # Errors
///
/// Returns [`crate::error::GraphInternalError::Decode`] if the token is malformed or
/// required claims are missing.
pub fn parse(id_token: &str) -> Result<IdTokenClaims, crate::error::GraphInternalError> {
    let parts: Vec<&str> = id_token.splitn(3, '.').collect();
    if parts.len() != 3 {
        return Err(crate::error::GraphInternalError::Decode {
            detail: "id_token does not have 3 dot-separated segments".to_owned(),
        });
    }

    let payload_b64 = parts[1];
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload_bytes =
        engine
            .decode(payload_b64)
            .map_err(|e| crate::error::GraphInternalError::Decode {
                detail: format!("id_token payload base64 decode failed: {e}"),
            })?;

    let claims: serde_json::Value = serde_json::from_slice(&payload_bytes).map_err(|e| {
        crate::error::GraphInternalError::Decode {
            detail: format!("id_token payload JSON parse failed: {e}"),
        }
    })?;

    let tid = string_claim(&claims, "tid")?;
    let oid = string_claim(&claims, "oid")?;

    // `preferred_username` is the standard OIDC claim; Microsoft also uses `upn` in some flows.
    let upn = claims
        .get("preferred_username")
        .or_else(|| claims.get("upn"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    let display_name = claims
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    let kind = if tid == CONSUMER_TENANT_ID {
        AccountKind::Personal
    } else {
        AccountKind::Business
    };

    Ok(IdTokenClaims {
        tid,
        oid,
        upn,
        display_name,
        kind,
    })
}

fn string_claim(
    claims: &serde_json::Value,
    key: &str,
) -> Result<String, crate::error::GraphInternalError> {
    claims
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| crate::error::GraphInternalError::Decode {
            detail: format!("id_token missing required claim '{key}'"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_token(payload: &serde_json::Value) -> String {
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = engine.encode(r#"{"alg":"RS256","typ":"JWT"}"#);
        let body = engine.encode(payload.to_string());
        format!("{header}.{body}.fakesig")
    }

    #[test]
    fn personal_account_tid_yields_personal_kind() {
        let token = make_token(&serde_json::json!({
            "tid": CONSUMER_TENANT_ID,
            "oid": "oid-123",
            "preferred_username": "alice@outlook.com",
            "name": "Alice"
        }));
        let claims = parse(&token).unwrap();
        assert_eq!(claims.kind, AccountKind::Personal);
        assert_eq!(claims.upn, "alice@outlook.com");
        assert_eq!(claims.display_name, "Alice");
    }

    #[test]
    fn business_account_tid_yields_business_kind() {
        let token = make_token(&serde_json::json!({
            "tid": "11111111-1111-1111-1111-111111111111",
            "oid": "oid-456",
            "preferred_username": "bob@company.com",
            "name": "Bob"
        }));
        let claims = parse(&token).unwrap();
        assert_eq!(claims.kind, AccountKind::Business);
        assert_eq!(claims.upn, "bob@company.com");
    }

    #[test]
    fn malformed_token_returns_decode_error() {
        let err = parse("not.valid").unwrap_err();
        assert!(
            matches!(err, crate::error::GraphInternalError::Decode { .. }),
            "expected Decode, got {err:?}"
        );
    }

    #[test]
    fn upn_falls_back_to_upn_claim() {
        let token = make_token(&serde_json::json!({
            "tid": "11111111-1111-1111-1111-111111111111",
            "oid": "oid-789",
            "upn": "carol@example.com",
            "name": "Carol"
        }));
        let claims = parse(&token).unwrap();
        assert_eq!(claims.upn, "carol@example.com");
    }
}
