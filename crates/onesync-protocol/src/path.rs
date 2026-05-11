//! Path newtypes enforcing the spec's path discipline.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// Maximum allowed byte length for any path value.
pub const MAX_PATH_BYTES: usize = 1024;

/// A validated, NFC-normalised relative path.
///
/// A `RelPath` must not be empty, must not start with `/`, must not contain
/// `..` components, must not contain embedded NUL bytes, and must not exceed
/// [`MAX_PATH_BYTES`] bytes. The stored string is always in Unicode NFC form.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

impl RelPath {
    /// Returns the path as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A validated absolute path.
///
/// An `AbsPath` must start with `/`, must not be empty, must not contain `..`
/// components, must not contain embedded NUL bytes, and must not exceed
/// [`MAX_PATH_BYTES`] bytes.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AbsPath(String);

impl AbsPath {
    /// Returns the path as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Errors returned when parsing a [`RelPath`] or [`AbsPath`].
#[derive(Debug, thiserror::Error)]
pub enum PathParseError {
    /// The supplied string was empty.
    #[error("path is empty")]
    Empty,
    /// The supplied string exceeds the byte limit.
    #[error("path exceeds {limit}-byte limit (got {got})")]
    TooLong {
        /// Actual byte length of the supplied string.
        got: usize,
        /// The maximum allowed byte length.
        limit: usize,
    },
    /// The supplied string contains an embedded NUL byte.
    #[error("path contains embedded NUL")]
    EmbeddedNul,
    /// The supplied string contains a `..` component.
    #[error("path contains a `..` component")]
    ParentComponent,
    /// A relative path was supplied where one must not start with `/`.
    #[error("relative path must not start with '/'")]
    LeadingSlash,
    /// An absolute path was supplied without a leading `/`.
    #[error("absolute path must start with '/'")]
    NotAbsolute,
}

fn validate_common(s: &str) -> Result<(), PathParseError> {
    if s.is_empty() {
        return Err(PathParseError::Empty);
    }
    if s.len() > MAX_PATH_BYTES {
        return Err(PathParseError::TooLong {
            got: s.len(),
            limit: MAX_PATH_BYTES,
        });
    }
    if s.contains('\0') {
        return Err(PathParseError::EmbeddedNul);
    }
    for component in s.split('/') {
        if component == ".." {
            return Err(PathParseError::ParentComponent);
        }
    }
    Ok(())
}

impl FromStr for RelPath {
    type Err = PathParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.starts_with('/') {
            return Err(PathParseError::LeadingSlash);
        }
        let nfc: String = s.nfc().collect();
        validate_common(&nfc)?;
        Ok(Self(nfc))
    }
}

impl FromStr for AbsPath {
    type Err = PathParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if !s.starts_with('/') {
            return Err(PathParseError::NotAbsolute);
        }
        validate_common(s)?;
        Ok(Self(s.to_owned()))
    }
}

impl TryFrom<String> for RelPath {
    type Error = PathParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}
impl TryFrom<String> for AbsPath {
    type Error = PathParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}
impl From<RelPath> for String {
    fn from(p: RelPath) -> Self {
        p.0
    }
}
impl From<AbsPath> for String {
    fn from(p: AbsPath) -> Self {
        p.0
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl fmt::Debug for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RelPath({:?})", self.0)
    }
}
impl fmt::Display for AbsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl fmt::Debug for AbsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AbsPath({:?})", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_path_accepts_a_normal_relative_path() {
        let p: RelPath = "Documents/notes.md".parse().expect("parses");
        assert_eq!(p.as_str(), "Documents/notes.md");
    }

    #[test]
    fn rel_path_rejects_leading_slash() {
        assert!("/Documents/notes.md".parse::<RelPath>().is_err());
    }

    #[test]
    fn rel_path_rejects_dotdot() {
        assert!("Documents/../etc/passwd".parse::<RelPath>().is_err());
    }

    #[test]
    fn rel_path_rejects_embedded_nul() {
        assert!("Documents\0/foo".parse::<RelPath>().is_err());
    }

    #[test]
    fn rel_path_normalises_to_nfc() {
        // NFD: e + combining acute accent
        let nfd = "caf\u{0065}\u{0301}";
        let nfc = "caf\u{00E9}";
        let p: RelPath = nfd.parse().expect("parses");
        assert_eq!(p.as_str(), nfc);
    }

    #[test]
    fn abs_path_accepts_an_absolute_macos_path() {
        let p: AbsPath = "/Users/alice/OneDrive".parse().expect("parses");
        assert_eq!(p.as_str(), "/Users/alice/OneDrive");
    }

    #[test]
    fn abs_path_rejects_relative() {
        assert!("Users/alice/OneDrive".parse::<AbsPath>().is_err());
    }
}
