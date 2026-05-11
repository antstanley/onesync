//! Typed identifiers of the form `<prefix>_<ulid>`.
//!
//! The base regex enforced is `^[a-z]{2,4}_[0-9A-HJKMNP-TV-Z]{26}$`, matching the
//! schema's `Id` `$def`.

use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// A typed-prefix tag for [`Id<T>`].
pub trait IdPrefix {
    /// The literal prefix string, without the trailing underscore.
    const PREFIX: &'static str;
}

/// A `<prefix>_<ulid>` identifier with compile-time prefix discipline.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Id<T: IdPrefix> {
    ulid: Ulid,
    _marker: PhantomData<fn() -> T>,
}

impl<T: IdPrefix> Id<T> {
    /// Construct an [`Id`] directly from a [`Ulid`] without parsing.
    #[must_use]
    pub fn from_ulid(ulid: Ulid) -> Self {
        Self {
            ulid,
            _marker: PhantomData,
        }
    }

    /// Return the inner [`Ulid`].
    #[must_use]
    pub const fn ulid(&self) -> Ulid {
        self.ulid
    }
}

impl<T: IdPrefix> fmt::Display for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}_{}", T::PREFIX, self.ulid)
    }
}

impl<T: IdPrefix> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Errors returned when parsing an [`Id`] from a string.
#[derive(Debug, thiserror::Error)]
pub enum IdParseError {
    /// The string contains no `_` separator between prefix and ULID body.
    #[error("missing underscore separator")]
    MissingSeparator,
    /// The prefix found in the string does not match the expected prefix for this `Id` type.
    #[error("wrong prefix: got {got:?}, expected {expected:?}")]
    WrongPrefix {
        /// The prefix actually found.
        got: String,
        /// The prefix that was expected.
        expected: &'static str,
    },
    /// The 26-character body after the `_` is not a valid ULID.
    #[error("malformed ULID body: {source}")]
    MalformedUlid {
        /// The underlying decode error from the `ulid` crate.
        #[source]
        source: ulid::DecodeError,
    },
}

impl<T: IdPrefix> FromStr for Id<T> {
    type Err = IdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (prefix, body) = s.split_once('_').ok_or(IdParseError::MissingSeparator)?;
        if prefix != T::PREFIX {
            return Err(IdParseError::WrongPrefix {
                got: prefix.to_owned(),
                expected: T::PREFIX,
            });
        }
        let ulid = body
            .parse::<Ulid>()
            .map_err(|source| IdParseError::MalformedUlid { source })?;
        Ok(Self::from_ulid(ulid))
    }
}

impl<T: IdPrefix> Serialize for Id<T> {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_str(self)
    }
}

impl<'de, T: IdPrefix> Deserialize<'de> for Id<T> {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = <String as Deserialize>::deserialize(de)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Local-only tag used in tests; Task 7 introduces the project-wide tags
    // (`PairTag`, `AccountTag`, …) at module level. Keep this private to the test mod
    // so the two never collide.
    struct TestTag;
    impl IdPrefix for TestTag {
        const PREFIX: &'static str = "pair";
    }

    #[test]
    fn it_round_trips_a_valid_id_through_string() {
        let original = "pair_01J8X7CFGMZG7Y4DC0VA8DZW2H";
        let id: Id<TestTag> = original.parse().expect("parses");
        assert_eq!(id.to_string(), original);
    }

    #[test]
    fn it_rejects_an_id_with_the_wrong_prefix() {
        let bad = "acct_01J8X7CFGMZG7Y4DC0VA8DZW2H";
        let err = bad.parse::<Id<TestTag>>().expect_err("rejects");
        assert!(matches!(err, IdParseError::WrongPrefix { .. }));
    }

    #[test]
    fn it_rejects_an_id_with_a_malformed_ulid_body() {
        let bad = "pair_NOTAULID0000000000000000ZZ";
        let err = bad.parse::<Id<TestTag>>().expect_err("rejects");
        assert!(matches!(err, IdParseError::MalformedUlid { .. }));
    }
}
