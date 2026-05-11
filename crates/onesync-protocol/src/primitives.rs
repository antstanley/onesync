//! Primitive newtypes shared across the protocol crate.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// ISO-8601 UTC timestamp with seconds precision or finer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timestamp(pub DateTime<Utc>);

impl Timestamp {
    /// Wrap a [`DateTime<Utc>`] in a [`Timestamp`].
    #[must_use]
    pub const fn from_datetime(dt: DateTime<Utc>) -> Self {
        Self(dt)
    }

    /// Unwrap the inner [`DateTime<Utc>`].
    #[must_use]
    pub const fn into_inner(self) -> DateTime<Utc> {
        self.0
    }
}

/// BLAKE3 content digest (64 lowercase hex characters).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    /// Construct a [`ContentHash`] directly from a raw 32-byte array.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Return a reference to the underlying 32-byte array.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Errors returned when parsing a [`ContentHash`] from a hex string.
#[derive(Debug, thiserror::Error)]
pub enum ContentHashParseError {
    /// The input string was not exactly 64 hex characters.
    #[error("expected 64 hex characters, got {got}")]
    WrongLength {
        /// The actual length of the input string.
        got: usize,
    },
    /// A non-hex character was found in the input.
    #[error("non-hex character at index {index}")]
    NonHex {
        /// The byte index of the offending character.
        index: usize,
    },
}

impl FromStr for ContentHash {
    type Err = ContentHashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 {
            return Err(ContentHashParseError::WrongLength { got: s.len() });
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = decode_nibble(chunk[0])
                .map_err(|()| ContentHashParseError::NonHex { index: i * 2 })?;
            let lo = decode_nibble(chunk[1])
                .map_err(|()| ContentHashParseError::NonHex { index: i * 2 + 1 })?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

const fn decode_nibble(b: u8) -> Result<u8, ()> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(()),
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl Serialize for ContentHash {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = <String as Deserialize>::deserialize(de)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

macro_rules! opaque_string {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(pub String);

        impl $name {
            /// Construct from any value that converts into a [`String`].
            #[must_use]
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            /// Return the value as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

opaque_string!(ETag, "`OneDrive` ETag/cTag value (opaque).");
opaque_string!(DriveItemId, "`OneDrive` driveItem id (opaque).");
opaque_string!(DriveId, "`OneDrive` drive id (opaque).");
opaque_string!(
    DeltaCursor,
    "Opaque cursor token returned by Microsoft Graph /delta."
);
opaque_string!(
    KeychainRef,
    "Pointer into the macOS Keychain for a refresh-token entry."
);

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn timestamp_round_trips_through_json() {
        let json = json!("2026-05-11T18:00:00Z");
        let ts: Timestamp = serde_json::from_value(json.clone()).expect("parses");
        assert_eq!(serde_json::to_value(ts).expect("serializes"), json);
    }

    #[test]
    fn content_hash_accepts_64_hex_chars() {
        let h = "0".repeat(64);
        let hash: ContentHash = h.parse().expect("parses");
        assert_eq!(hash.to_string(), h);
    }

    #[test]
    fn content_hash_rejects_non_hex_or_wrong_length() {
        assert!("xy".repeat(32).parse::<ContentHash>().is_err());
        assert!("0".repeat(63).parse::<ContentHash>().is_err());
        assert!("0".repeat(65).parse::<ContentHash>().is_err());
    }
}
