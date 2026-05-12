//! PKCE verifier/challenge generation per RFC 7636.

use base64::Engine as _;
use sha2::{Digest, Sha256};

/// A PKCE verifier/challenge pair.
#[derive(Debug, Clone)]
pub struct PkcePair {
    /// Base64url-no-padding encoded verifier (43 chars, 32 random bytes).
    pub verifier: String,
    /// Base64url-no-padding encoded SHA-256 of the verifier.
    pub challenge: String,
}

/// Generate a fresh [`PkcePair`] using 32 random bytes from the OS CSPRNG.
///
/// # Panics
///
/// Panics if the OS CSPRNG is unavailable (should never happen on any supported platform).
#[must_use]
pub fn generate() -> PkcePair {
    let mut raw = [0u8; 32];
    // LINT: getrandom failure is unrecoverable OS error at init time.
    #[allow(clippy::expect_used)]
    getrandom::getrandom(&mut raw).expect("getrandom should succeed on all supported platforms");
    from_raw_bytes(&raw)
}

/// Build a [`PkcePair`] from a 32-byte slice (exposed for deterministic tests).
#[must_use]
pub fn from_raw_bytes(raw: &[u8; 32]) -> PkcePair {
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let verifier = engine.encode(raw);
    let hash = Sha256::digest(verifier.as_bytes());
    let challenge = engine.encode(hash);
    PkcePair {
        verifier,
        challenge,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifier_matches_rfc_7636_char_set_and_length() {
        let pair = generate();
        assert!(
            pair.verifier.len() >= 43 && pair.verifier.len() <= 128,
            "verifier length out of range: {}",
            pair.verifier.len()
        );
        assert!(
            pair.verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '~' | '.' | '-')),
            "verifier contains invalid chars: {}",
            pair.verifier
        );
    }

    #[test]
    fn challenge_is_43_chars() {
        let pair = generate();
        assert_eq!(
            pair.challenge.len(),
            43,
            "challenge should be 43 base64url chars"
        );
    }

    /// RFC 7636 Appendix B round-trip test.
    ///
    /// The spec uses a specific verifier string, not raw bytes. We derive
    /// the challenge from the verifier bytes directly to mirror the spec example.
    #[test]
    fn rfc_7636_appendix_b_known_vector() {
        // The RFC 7636 Appendix B example:
        // verifier  = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        // challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected_challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

        let hash = Sha256::digest(verifier.as_bytes());
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let challenge = engine.encode(hash);

        assert_eq!(
            challenge, expected_challenge,
            "PKCE challenge does not match RFC 7636 Appendix B"
        );
    }

    #[test]
    fn generate_produces_distinct_pairs() {
        let a = generate();
        let b = generate();
        // Extremely unlikely to collide with 32 random bytes.
        assert_ne!(a.verifier, b.verifier);
    }
}
