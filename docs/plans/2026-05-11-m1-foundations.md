# onesync M1 — Foundations Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the Cargo workspace, the I/O-free crates (`onesync-protocol`, `onesync-core`, `onesync-time`), the limits module, and the port traits — everything an adapter or engine will depend on later.

**Architecture:** Three crates in a workspace. `onesync-protocol` owns serde-derived canonical types and their JSON-Schema parity. `onesync-core` owns port traits, limits, port-level error types, and zero I/O. `onesync-time` owns `Clock` and `IdGenerator` adapters (production + fakes). No `unsafe` anywhere in M1.

**Tech Stack:** Rust 1.95.0 (via `rust-toolchain.toml`), edition 2024. Dependencies: `serde`, `serde_json`, `thiserror`, `ulid`, `chrono` (UTC, serde feature), `unicode-normalization`, `jsonschema`, `proptest`. Async-related deps (`async-trait`, `tokio`) appear only as port-trait annotations.

VCS: `jj` colocated. Per-task commits use Conventional Commits format with a trailing `Co-Authored-By` line.

---

## Pre-flight (zero engineer work, but read before starting)

- The repo is already initialised: `jj git init --colocate` was run on `main`, remote `origin` tracks `https://github.com/antstanley/onesync.git`.
- The spec is in [`docs/spec/`](../spec/); the JSON Schema sidecar at [`docs/spec/canonical-types.schema.json`](../spec/canonical-types.schema.json) is the authoritative shape — your Rust types match it, not the other way around.
- The cross-project development guidelines are at the gist linked from [`docs/spec/09-development-guidelines.md`](../spec/09-development-guidelines.md). Read them. Tiger Style applies.
- Every task ends with one commit. The plan never batches multiple tasks into one commit.
- Run `cargo install cargo-nextest --locked` once before starting if it isn't already on your machine.

---

## File map (what M1 creates)

```
onesync/
├── Cargo.toml                                 # workspace root
├── rust-toolchain.toml
├── clippy.toml
├── .editorconfig
├── crates/
│   ├── onesync-protocol/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── id.rs
│   │       ├── primitives.rs                  # Timestamp, ContentHash, etc.
│   │       ├── path.rs                        # RelPath, AbsPath
│   │       ├── enums.rs
│   │       ├── file_side.rs
│   │       ├── account.rs
│   │       ├── pair.rs
│   │       ├── file_entry.rs
│   │       ├── file_op.rs
│   │       ├── conflict.rs
│   │       ├── sync_run.rs
│   │       ├── audit.rs
│   │       ├── config.rs
│   │       ├── errors.rs                      # ErrorEnvelope, RpcError
│   │       ├── handles.rs                     # SyncRunHandle, SubscriptionAck, PairStatusDetail, Diagnostics, SyncRunDetail
│   │       └── schema_test.rs                 # under #[cfg(test)]
│   ├── onesync-core/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── limits.rs
│   │       └── ports/
│   │           ├── mod.rs
│   │           ├── state.rs
│   │           ├── local_fs.rs
│   │           ├── remote_drive.rs
│   │           ├── token_vault.rs
│   │           ├── clock.rs
│   │           ├── id_generator.rs
│   │           └── audit_sink.rs
│   └── onesync-time/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── system_clock.rs
│           ├── ulid_generator.rs
│           └── fakes.rs
└── docs/spec/canonical-types.schema.json      # (already exists, read-only here)
```

---

## Task 1: Workspace, toolchain, lints

**Files:**
- Create: `Cargo.toml`
- Create: `rust-toolchain.toml`
- Create: `clippy.toml`
- Create: `.editorconfig`

- [ ] **Step 1.1: Create `rust-toolchain.toml`**

```toml
[toolchain]
channel = "1.95.0"
components = ["rustfmt", "clippy"]
targets = ["aarch64-apple-darwin", "x86_64-apple-darwin"]
```

- [ ] **Step 1.2: Create `clippy.toml`**

```toml
avoid-breaking-exported-api = false
disallowed-methods = [
  { path = "std::env::var", reason = "Use InstanceConfig; env reads belong in onesync-daemon startup only." },
  { path = "chrono::Utc::now", reason = "Use the Clock port." },
  { path = "ulid::Ulid::new", reason = "Use the IdGenerator port." },
]
```

- [ ] **Step 1.3: Create `.editorconfig`**

```ini
root = true

[*]
charset = utf-8
end_of_line = lf
indent_style = space
indent_size = 4
insert_final_newline = true
trim_trailing_whitespace = true

[*.md]
trim_trailing_whitespace = false

[*.{toml,yaml,yml,json}]
indent_size = 2
```

- [ ] **Step 1.4: Create the workspace `Cargo.toml`**

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
edition = "2024"
rust-version = "1.95.0"
license = "MIT OR Apache-2.0"
authors = ["onesync contributors"]
repository = "https://github.com/antstanley/onesync"

[workspace.lints.rust]
unsafe_code = "forbid"
missing_docs = "warn"

[workspace.lints.clippy]
pedantic = { level = "warn", priority = -1 }
nursery  = { level = "warn", priority = -1 }
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
todo = "deny"
unimplemented = "deny"
missing_errors_doc = "allow"
missing_panics_doc = "allow"

[workspace.dependencies]
serde         = { version = "1", features = ["derive"] }
serde_json    = "1"
thiserror     = "1"
ulid          = { version = "1", features = ["serde"] }
chrono        = { version = "0.4", default-features = false, features = ["clock", "serde", "std"] }
unicode-normalization = "0.1"
jsonschema    = "0.18"
async-trait   = "0.1"
proptest      = "1"
```

- [ ] **Step 1.5: Run `cargo check --workspace`**

Run: `cargo check --workspace`
Expected: `warning: virtual workspace defaulting to resolver "3" ...` — no member crates yet but workspace parses.

- [ ] **Step 1.6: Commit**

```bash
jj describe -m "chore: initialise cargo workspace and lint config

Co-Authored-By: <name> <email>"
jj new
```

---

## Task 2: `onesync-protocol` crate skeleton + `Id<T>`

**Files:**
- Create: `crates/onesync-protocol/Cargo.toml`
- Create: `crates/onesync-protocol/src/lib.rs`
- Create: `crates/onesync-protocol/src/id.rs`

- [ ] **Step 2.1: Create `crates/onesync-protocol/Cargo.toml`**

```toml
[package]
name = "onesync-protocol"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
serde         = { workspace = true }
serde_json    = { workspace = true }
thiserror     = { workspace = true }
ulid          = { workspace = true }
chrono        = { workspace = true }
unicode-normalization = { workspace = true }

[dev-dependencies]
jsonschema    = { workspace = true }
proptest      = { workspace = true }
```

- [ ] **Step 2.2: Create `crates/onesync-protocol/src/lib.rs`**

```rust
//! Canonical onesync domain types.
//!
//! Every public type round-trips through `serde_json` and validates against
//! [`docs/spec/canonical-types.schema.json`](../../../docs/spec/canonical-types.schema.json).

#![forbid(unsafe_code)]

pub mod id;
```

- [ ] **Step 2.3: Write the failing test for `Id<T>` parse/render round-trip**

Append to `crates/onesync-protocol/src/id.rs`:

```rust
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
```

- [ ] **Step 2.4: Run the test (it must fail)**

Run: `cargo nextest run -p onesync-protocol id::tests`
Expected: compile error — `Id`, `IdPrefix`, `IdParseError` undefined.

- [ ] **Step 2.5: Implement `Id<T>`**

Prepend to `crates/onesync-protocol/src/id.rs`:

```rust
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
    #[must_use]
    pub fn from_ulid(ulid: Ulid) -> Self {
        Self { ulid, _marker: PhantomData }
    }

    #[must_use]
    pub fn ulid(&self) -> Ulid {
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

#[derive(Debug, thiserror::Error)]
pub enum IdParseError {
    #[error("missing underscore separator")]
    MissingSeparator,
    #[error("wrong prefix: got {got:?}, expected {expected:?}")]
    WrongPrefix { got: String, expected: &'static str },
    #[error("malformed ULID body: {source}")]
    MalformedUlid {
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
```

- [ ] **Step 2.6: Run the test (it must pass)**

Run: `cargo nextest run -p onesync-protocol id::tests`
Expected: 3 passed, 0 failed.

- [ ] **Step 2.7: Run lints**

Run: `cargo clippy -p onesync-protocol --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: no output, exit 0.

- [ ] **Step 2.8: Commit**

```bash
jj describe -m "feat(protocol): typed Id<T> with prefix-discipline parse and serde

Co-Authored-By: <name> <email>"
jj new
```

---

## Task 3: Primitive newtypes (`Timestamp`, `ContentHash`, opaque strings)

**Files:**
- Create: `crates/onesync-protocol/src/primitives.rs`
- Modify: `crates/onesync-protocol/src/lib.rs:5` (add `pub mod primitives;`)

- [ ] **Step 3.1: Write failing tests**

Create `crates/onesync-protocol/src/primitives.rs` with only the test module first:

```rust
#![allow(dead_code)] // implementations land in step 3.3

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn timestamp_round_trips_through_json() {
        let json = json!("2026-05-11T18:00:00Z");
        let ts: Timestamp = serde_json::from_value(json.clone()).expect("parses");
        assert_eq!(serde_json::to_value(&ts).expect("serializes"), json);
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
```

- [ ] **Step 3.2: Run tests, watch them fail**

Run: `cargo nextest run -p onesync-protocol primitives::tests`
Expected: compile errors — `Timestamp`, `ContentHash` undefined.

- [ ] **Step 3.3: Implement primitives**

Prepend to `crates/onesync-protocol/src/primitives.rs`:

```rust
//! Primitive newtypes shared across the protocol crate.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// ISO-8601 UTC timestamp with seconds precision or finer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timestamp(pub DateTime<Utc>);

impl Timestamp {
    #[must_use]
    pub fn from_datetime(dt: DateTime<Utc>) -> Self {
        Self(dt)
    }

    #[must_use]
    pub fn into_inner(self) -> DateTime<Utc> {
        self.0
    }
}

/// BLAKE3 content digest (64 lowercase hex characters).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ContentHashParseError {
    #[error("expected 64 hex characters, got {got}")]
    WrongLength { got: usize },
    #[error("non-hex character at index {index}")]
    NonHex { index: usize },
}

impl FromStr for ContentHash {
    type Err = ContentHashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 64 {
            return Err(ContentHashParseError::WrongLength { got: s.len() });
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = decode_nibble(chunk[0]).map_err(|()| ContentHashParseError::NonHex {
                index: i * 2,
            })?;
            let lo = decode_nibble(chunk[1]).map_err(|()| ContentHashParseError::NonHex {
                index: i * 2 + 1,
            })?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

fn decode_nibble(b: u8) -> Result<u8, ()> {
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
            #[must_use]
            pub fn new(s: impl Into<String>) -> Self {
                Self(s.into())
            }
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

opaque_string!(ETag,         "OneDrive ETag/cTag value (opaque).");
opaque_string!(DriveItemId,  "OneDrive driveItem id (opaque).");
opaque_string!(DriveId,      "OneDrive drive id (opaque).");
opaque_string!(DeltaCursor,  "Opaque cursor token returned by Microsoft Graph /delta.");
opaque_string!(KeychainRef,  "Pointer into the macOS Keychain for a refresh-token entry.");
```

- [ ] **Step 3.4: Wire the module into `lib.rs`**

Modify `crates/onesync-protocol/src/lib.rs`:

```rust
pub mod id;
pub mod primitives;
```

- [ ] **Step 3.5: Run tests + lints**

Run:
```
cargo nextest run -p onesync-protocol
cargo clippy -p onesync-protocol --all-targets -- -D warnings
cargo fmt --all -- --check
```
Expected: 6 tests passed (3 id + 3 primitives), no clippy or fmt diffs.

- [ ] **Step 3.6: Commit**

```bash
jj commit -m "feat(protocol): Timestamp, ContentHash, and opaque-string newtypes

Co-Authored-By: <name> <email>"
```

---

## Task 4: Path newtypes (`RelPath`, `AbsPath`)

**Files:**
- Create: `crates/onesync-protocol/src/path.rs`
- Modify: `crates/onesync-protocol/src/lib.rs`

- [ ] **Step 4.1: Write failing tests**

```rust
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
```

- [ ] **Step 4.2: Run tests, watch them fail**

Run: `cargo nextest run -p onesync-protocol path::tests`
Expected: compile errors.

- [ ] **Step 4.3: Implement `RelPath` and `AbsPath`**

Prepend to `crates/onesync-protocol/src/path.rs`:

```rust
//! Path newtypes enforcing the spec's path discipline.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

pub const MAX_PATH_BYTES: usize = 1024;

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

impl RelPath {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AbsPath(String);

impl AbsPath {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PathParseError {
    #[error("path is empty")]
    Empty,
    #[error("path exceeds {limit}-byte limit (got {got})")]
    TooLong { got: usize, limit: usize },
    #[error("path contains embedded NUL")]
    EmbeddedNul,
    #[error("path contains a `..` component")]
    ParentComponent,
    #[error("relative path must not start with '/'")]
    LeadingSlash,
    #[error("absolute path must start with '/'")]
    NotAbsolute,
}

fn validate_common(s: &str) -> Result<(), PathParseError> {
    if s.is_empty() {
        return Err(PathParseError::Empty);
    }
    if s.len() > MAX_PATH_BYTES {
        return Err(PathParseError::TooLong { got: s.len(), limit: MAX_PATH_BYTES });
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
    fn try_from(s: String) -> Result<Self, Self::Error> { s.parse() }
}
impl TryFrom<String> for AbsPath {
    type Error = PathParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> { s.parse() }
}
impl From<RelPath> for String { fn from(p: RelPath) -> Self { p.0 } }
impl From<AbsPath> for String { fn from(p: AbsPath) -> Self { p.0 } }

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}
impl fmt::Debug for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "RelPath({:?})", self.0) }
}
impl fmt::Display for AbsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}
impl fmt::Debug for AbsPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "AbsPath({:?})", self.0) }
}
```

- [ ] **Step 4.4: Add module to `lib.rs`**

Modify `crates/onesync-protocol/src/lib.rs`:

```rust
pub mod id;
pub mod path;
pub mod primitives;
```

- [ ] **Step 4.5: Run tests + lints**

Run: `cargo nextest run -p onesync-protocol && cargo clippy -p onesync-protocol --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: all paths tests + previous tests pass, no clippy/fmt issues.

- [ ] **Step 4.6: Commit**

```bash
jj commit -m "feat(protocol): RelPath/AbsPath with NFC normalisation and validation

Co-Authored-By: <name> <email>"
```

---

## Task 5: Enums

**Files:**
- Create: `crates/onesync-protocol/src/enums.rs`
- Modify: `crates/onesync-protocol/src/lib.rs`

- [ ] **Step 5.1: Write failing round-trip tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! round_trip {
        ($ty:ty, $variant:expr, $wire:expr) => {{
            let v: $ty = $variant;
            let json = serde_json::to_string(&v).expect("serialize");
            assert_eq!(json, format!("\"{}\"", $wire));
            let back: $ty = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, v);
        }};
    }

    #[test]
    fn file_kind_round_trip() {
        round_trip!(FileKind, FileKind::File, "file");
        round_trip!(FileKind, FileKind::Directory, "directory");
    }

    #[test]
    fn pair_status_round_trip_all_variants() {
        round_trip!(PairStatus, PairStatus::Initializing, "initializing");
        round_trip!(PairStatus, PairStatus::Active, "active");
        round_trip!(PairStatus, PairStatus::Paused, "paused");
        round_trip!(PairStatus, PairStatus::Errored, "errored");
        round_trip!(PairStatus, PairStatus::Removed, "removed");
    }

    #[test]
    fn file_op_kind_does_not_include_resolve_conflict() {
        let raw = "\"resolve_conflict\"";
        assert!(serde_json::from_str::<FileOpKind>(raw).is_err());
    }
}
```

- [ ] **Step 5.2: Run, watch fail**

Run: `cargo nextest run -p onesync-protocol enums::tests`
Expected: compile errors.

- [ ] **Step 5.3: Implement enums**

Prepend to `crates/onesync-protocol/src/enums.rs`:

```rust
//! String-valued enums shared across the protocol.
//!
//! Every enum is `serde(rename_all = "snake_case")` to match the JSON Schema.

use serde::{Deserialize, Serialize};

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant,)+
        }
    };
}

string_enum!(FileKind         { File, Directory });
string_enum!(AccountKind      { Personal, Business });
string_enum!(PairStatus       { Initializing, Active, Paused, Errored, Removed });
string_enum!(FileSyncState    { Clean, Dirty, PendingUpload, PendingDownload, PendingConflict, InFlight });
string_enum!(FileOpKind       { Upload, Download, LocalDelete, RemoteDelete, LocalMkdir, RemoteMkdir, LocalRename, RemoteRename });
string_enum!(FileOpStatus     { Enqueued, InProgress, Backoff, Success, Failed });
string_enum!(RunTrigger       { Scheduled, LocalEvent, RemoteWebhook, CliForce, BackoffRetry });
string_enum!(RunOutcome       { Success, PartialFailure, Aborted });
string_enum!(ConflictSide     { Local, Remote });
string_enum!(ConflictResolution { Auto, Manual });
string_enum!(AuditLevel       { Info, Warn, Error });
string_enum!(LogLevel         { Info, Debug, Trace });
```

- [ ] **Step 5.4: Wire into `lib.rs`**

Modify `crates/onesync-protocol/src/lib.rs`:

```rust
pub mod enums;
pub mod id;
pub mod path;
pub mod primitives;
```

- [ ] **Step 5.5: Run, lint, commit**

```
cargo nextest run -p onesync-protocol
cargo clippy -p onesync-protocol --all-targets -- -D warnings
cargo fmt --all -- --check
jj commit -m "feat(protocol): canonical enums

Co-Authored-By: <name> <email>"
```

---

## Task 6: `FileSide` struct

**Files:**
- Create: `crates/onesync-protocol/src/file_side.rs`
- Modify: `crates/onesync-protocol/src/lib.rs`

- [ ] **Step 6.1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::FileKind;
    use crate::primitives::{ContentHash, Timestamp};
    use chrono::TimeZone;

    fn side(size: u64, hash: &str, mtime_secs: i64) -> FileSide {
        FileSide {
            kind: FileKind::File,
            size_bytes: size,
            content_hash: Some(hash.parse().unwrap()),
            mtime: Timestamp::from_datetime(chrono::Utc.timestamp_opt(mtime_secs, 0).unwrap()),
            etag: None,
            remote_item_id: None,
        }
    }

    #[test]
    fn equality_ignores_mtime_and_etag() {
        let a = side(10, &"00".repeat(32), 100);
        let mut b = side(10, &"00".repeat(32), 9_999);
        b.etag = Some(crate::primitives::ETag::new("etag-x"));
        assert!(a.identifies_same_content_as(&b));
    }

    #[test]
    fn equality_diverges_when_hash_differs() {
        let a = side(10, &"00".repeat(32), 100);
        let b = side(10, &"ff".repeat(32), 100);
        assert!(!a.identifies_same_content_as(&b));
    }
}
```

- [ ] **Step 6.2: Run, watch fail**

Run: `cargo nextest run -p onesync-protocol file_side::tests`
Expected: compile errors.

- [ ] **Step 6.3: Implement `FileSide`**

Prepend to `crates/onesync-protocol/src/file_side.rs`:

```rust
//! Snapshot of one side's view of a file at a point in time.

use serde::{Deserialize, Serialize};

use crate::enums::FileKind;
use crate::primitives::{ContentHash, DriveItemId, ETag, Timestamp};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSide {
    pub kind: FileKind,
    pub size_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<ContentHash>,
    pub mtime: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<ETag>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_item_id: Option<DriveItemId>,
}

impl FileSide {
    /// Content-equality: `kind`, `size_bytes`, and `content_hash` match.
    /// `mtime` and `etag` are metadata and not part of equality, matching the
    /// rule in [`docs/spec/03-sync-engine.md`](../../docs/spec/03-sync-engine.md).
    #[must_use]
    pub fn identifies_same_content_as(&self, other: &Self) -> bool {
        debug_assert!(
            self.kind == FileKind::Directory || self.content_hash.is_some()
                || other.kind == FileKind::Directory || other.content_hash.is_some(),
            "file sides ought to carry hashes for equality checks"
        );
        self.kind == other.kind
            && self.size_bytes == other.size_bytes
            && self.content_hash == other.content_hash
    }
}
```

- [ ] **Step 6.4: Wire into `lib.rs`**

```rust
pub mod enums;
pub mod file_side;
pub mod id;
pub mod path;
pub mod primitives;
```

- [ ] **Step 6.5: Run, lint, commit**

```
cargo nextest run -p onesync-protocol
cargo clippy -p onesync-protocol --all-targets -- -D warnings
cargo fmt --all -- --check
jj commit -m "feat(protocol): FileSide with content-only equality

Co-Authored-By: <name> <email>"
```

---

## Task 7: `Account` and `Pair`

**Files:**
- Create: `crates/onesync-protocol/src/account.rs`
- Create: `crates/onesync-protocol/src/pair.rs`
- Modify: `crates/onesync-protocol/src/lib.rs`

- [ ] **Step 7.1: Define ID tag types and write a failing schema round-trip test**

Append to `crates/onesync-protocol/src/id.rs`:

```rust
macro_rules! id_tag {
    ($tag:ident, $prefix:expr, $alias:ident) => {
        pub struct $tag;
        impl IdPrefix for $tag {
            const PREFIX: &'static str = $prefix;
        }
        pub type $alias = Id<$tag>;
    };
}

id_tag!(PairTag,    "pair", PairId);
id_tag!(AccountTag, "acct", AccountId);
id_tag!(ConflictTag,"cfl",  ConflictId);
id_tag!(SyncRunTag, "run",  SyncRunId);
id_tag!(FileOpTag,  "op",   FileOpId);
id_tag!(AuditTag,   "aud",  AuditEventId);
```

Create `crates/onesync-protocol/src/account.rs`:

```rust
//! Account entity.

use serde::{Deserialize, Serialize};

use crate::enums::AccountKind;
use crate::id::AccountId;
use crate::primitives::{DriveId, KeychainRef, Timestamp};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub kind: AccountKind,
    pub upn: String,
    pub tenant_id: String,
    pub drive_id: DriveId,
    pub display_name: String,
    pub keychain_ref: KeychainRef,
    pub scopes: Vec<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_round_trips_through_json() {
        let raw = serde_json::json!({
            "id": "acct_01J8X7CFGMZG7Y4DC0VA8DZW2H",
            "kind": "business",
            "upn": "alice@example.com",
            "tenant_id": "11111111-1111-1111-1111-111111111111",
            "drive_id": "drv-1",
            "display_name": "Alice",
            "keychain_ref": "kc-1",
            "scopes": ["Files.ReadWrite", "offline_access"],
            "created_at": "2026-05-11T10:00:00Z",
            "updated_at": "2026-05-11T10:00:00Z"
        });
        let acct: Account = serde_json::from_value(raw.clone()).expect("parses");
        let back = serde_json::to_value(&acct).expect("serializes");
        assert_eq!(back, raw);
    }
}
```

Create `crates/onesync-protocol/src/pair.rs`:

```rust
//! Pair entity.

use serde::{Deserialize, Serialize};

use crate::enums::PairStatus;
use crate::id::{AccountId, PairId};
use crate::path::AbsPath;
use crate::primitives::{DeltaCursor, DriveItemId, Timestamp};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    pub id: PairId,
    pub account_id: AccountId,
    pub local_path: AbsPath,
    pub remote_item_id: DriveItemId,
    pub remote_path: String,
    pub display_name: String,
    pub status: PairStatus,
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta_token: Option<DeltaCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errored_reason: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<Timestamp>,
    pub conflict_count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_round_trips_through_json() {
        let raw = serde_json::json!({
            "id": "pair_01J8X7CFGMZG7Y4DC0VA8DZW2H",
            "account_id": "acct_01J8X7CFGMZG7Y4DC0VA8DZW2H",
            "local_path": "/Users/alice/OneDrive",
            "remote_item_id": "drive-item-root",
            "remote_path": "/",
            "display_name": "OneDrive",
            "status": "active",
            "paused": false,
            "created_at": "2026-05-11T10:00:00Z",
            "updated_at": "2026-05-11T10:00:00Z",
            "conflict_count": 0
        });
        let pair: Pair = serde_json::from_value(raw.clone()).expect("parses");
        assert_eq!(serde_json::to_value(&pair).unwrap(), raw);
    }
}
```

- [ ] **Step 7.2: Wire modules**

```rust
pub mod account;
pub mod enums;
pub mod file_side;
pub mod id;
pub mod pair;
pub mod path;
pub mod primitives;
```

- [ ] **Step 7.3: Run, lint, commit**

```
cargo nextest run -p onesync-protocol
cargo clippy -p onesync-protocol --all-targets -- -D warnings
cargo fmt --all -- --check
jj commit -m "feat(protocol): Account and Pair entities with typed IDs

Co-Authored-By: <name> <email>"
```

---

## Task 8: Remaining entities (`ErrorEnvelope`, `FileEntry`, `FileOp`, `Conflict`, `SyncRun`, `AuditEvent`, `InstanceConfig`)

**Files:**
- Create: `crates/onesync-protocol/src/errors.rs`
- Create: `crates/onesync-protocol/src/file_entry.rs`
- Create: `crates/onesync-protocol/src/file_op.rs`
- Create: `crates/onesync-protocol/src/conflict.rs`
- Create: `crates/onesync-protocol/src/sync_run.rs`
- Create: `crates/onesync-protocol/src/audit.rs`
- Create: `crates/onesync-protocol/src/config.rs`
- Modify: `crates/onesync-protocol/src/lib.rs`

Each module follows the same pattern: declare the struct, mirror the schema's required/optional fields, add a `#[cfg(test)]` round-trip test using a JSON fixture. `errors.rs` lands first because `FileOp` depends on `ErrorEnvelope`.

- [ ] **Step 8.1: Implement `errors.rs`**

```rust
//! Structured error envelope used by ports, persisted ops, and the IPC.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub kind: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub context: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ErrorEnvelope>,
}
```

- [ ] **Step 8.2: Implement `file_entry.rs`**

```rust
//! Per-path sync state for one pair.

use serde::{Deserialize, Serialize};

use crate::enums::{FileKind, FileSyncState};
use crate::file_side::FileSide;
use crate::id::{FileOpId, PairId};
use crate::path::RelPath;
use crate::primitives::Timestamp;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub pair_id: PairId,
    pub relative_path: RelPath,
    pub kind: FileKind,
    pub sync_state: FileSyncState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<FileSide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<FileSide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced: Option<FileSide>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_op_id: Option<FileOpId>,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_entry_minimum_required_round_trips() {
        let raw = serde_json::json!({
            "pair_id": "pair_01J8X7CFGMZG7Y4DC0VA8DZW2H",
            "relative_path": "Documents/notes.md",
            "kind": "file",
            "sync_state": "clean",
            "updated_at": "2026-05-11T10:00:00Z"
        });
        let entry: FileEntry = serde_json::from_value(raw.clone()).expect("parses");
        assert_eq!(serde_json::to_value(&entry).unwrap(), raw);
    }
}
```

- [ ] **Step 8.3: Implement `file_op.rs`**

```rust
//! Discrete unit of sync work.

use serde::{Deserialize, Serialize};

use crate::enums::{FileOpKind, FileOpStatus};
use crate::errors::ErrorEnvelope;
use crate::id::{FileOpId, PairId, SyncRunId};
use crate::path::RelPath;
use crate::primitives::Timestamp;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOp {
    pub id: FileOpId,
    pub run_id: SyncRunId,
    pub pair_id: PairId,
    pub relative_path: RelPath,
    pub kind: FileOpKind,
    pub status: FileOpStatus,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<ErrorEnvelope>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
    pub enqueued_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
}
```

No dedicated unit test in this file — the schema-compliance test in Task 10 exercises `FileOp` end-to-end via JSON fixtures.

- [ ] **Step 8.4: Implement `conflict.rs`**

```rust
//! Persisted conflict record.

use serde::{Deserialize, Serialize};

use crate::enums::{ConflictResolution, ConflictSide};
use crate::file_side::FileSide;
use crate::id::{ConflictId, PairId};
use crate::path::RelPath;
use crate::primitives::Timestamp;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub id: ConflictId,
    pub pair_id: PairId,
    pub relative_path: RelPath,
    pub winner: ConflictSide,
    pub loser_relative_path: RelPath,
    pub local_side: FileSide,
    pub remote_side: FileSide,
    pub detected_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<ConflictResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
```

- [ ] **Step 8.5: Implement `sync_run.rs`**

```rust
//! Sync-cycle history entry.

use serde::{Deserialize, Serialize};

use crate::enums::{RunOutcome, RunTrigger};
use crate::id::{PairId, SyncRunId};
use crate::primitives::Timestamp;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRun {
    pub id: SyncRunId,
    pub pair_id: PairId,
    pub trigger: RunTrigger,
    pub started_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
    pub local_ops: u32,
    pub remote_ops: u32,
    pub bytes_uploaded: u64,
    pub bytes_downloaded: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<RunOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_detail: Option<String>,
}
```

- [ ] **Step 8.6: Implement `audit.rs`**

```rust
//! Structured-log entry persisted to the state store.

use serde::{Deserialize, Serialize};

use crate::enums::AuditLevel;
use crate::id::{AuditEventId, PairId};
use crate::primitives::Timestamp;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: AuditEventId,
    pub ts: Timestamp,
    pub level: AuditLevel,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_id: Option<PairId>,
    pub payload: serde_json::Map<String, serde_json::Value>,
}
```

- [ ] **Step 8.7: Implement `config.rs`**

```rust
//! Operator-tunable instance configuration.

use serde::{Deserialize, Serialize};

use crate::enums::LogLevel;
use crate::primitives::Timestamp;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub log_level: LogLevel,
    pub notify: bool,
    pub allow_metered: bool,
    pub min_free_gib: u32,
    pub updated_at: Timestamp,
}
```

- [ ] **Step 8.8: Wire into `lib.rs`**

```rust
pub mod account;
pub mod audit;
pub mod config;
pub mod conflict;
pub mod enums;
pub mod errors;
pub mod file_entry;
pub mod file_op;
pub mod file_side;
pub mod id;
pub mod pair;
pub mod path;
pub mod primitives;
pub mod sync_run;
```

- [ ] **Step 8.9: Compile, lint, commit**

```
cargo check -p onesync-protocol
cargo clippy -p onesync-protocol --all-targets -- -D warnings
cargo fmt --all -- --check
jj commit -m "feat(protocol): ErrorEnvelope plus FileEntry/FileOp/Conflict/SyncRun/AuditEvent/InstanceConfig

Co-Authored-By: <name> <email>"
```

`cargo nextest` will run after Task 10 wires in the schema test that covers these.

---

## Task 9: RPC response handles

**Files:**
- Create: `crates/onesync-protocol/src/handles.rs`
- Modify: `crates/onesync-protocol/src/lib.rs`

`ErrorEnvelope` and `RpcError` landed in Task 8. This task adds the multi-entity response wrappers used by RPC methods that return more than one record.

- [ ] **Step 9.1: Implement `handles.rs`**

```rust
//! RPC response shapes that wrap one or more entities.

use serde::{Deserialize, Serialize};

use crate::account::Account;
use crate::config::InstanceConfig;
use crate::conflict::Conflict;
use crate::file_op::FileOp;
use crate::id::SyncRunId;
use crate::pair::Pair;
use crate::sync_run::SyncRun;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRunHandle {
    pub run_id: SyncRunId,
    pub subscription_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionAck {
    pub subscription_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairStatusDetail {
    pub pair: Pair,
    pub in_flight_ops: Vec<FileOp>,
    pub recent_runs: Vec<SyncRun>,
    pub conflict_count: u32,
    pub queue_depth: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRunDetail {
    pub run: SyncRun,
    pub ops: Vec<FileOp>,
    pub conflicts: Vec<Conflict>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostics {
    pub version: String,
    pub schema_version: u32,
    pub uptime_s: u64,
    pub pairs: Vec<PairStatusDetail>,
    pub accounts: Vec<Account>,
    pub config: InstanceConfig,
    pub subscriptions: u32,
}
```

- [ ] **Step 9.2: Wire into `lib.rs`**

```rust
pub mod account;
pub mod audit;
pub mod config;
pub mod conflict;
pub mod enums;
pub mod errors;
pub mod file_entry;
pub mod file_op;
pub mod file_side;
pub mod handles;
pub mod id;
pub mod pair;
pub mod path;
pub mod primitives;
pub mod sync_run;
```

- [ ] **Step 9.3: Compile, lint, commit**

```
cargo check -p onesync-protocol
cargo clippy -p onesync-protocol --all-targets -- -D warnings
cargo fmt --all -- --check
jj commit -m "feat(protocol): RPC response handles (SyncRunHandle, Diagnostics, etc.)

Co-Authored-By: <name> <email>"
```

---

## Task 10: JSON-Schema compliance test

**Files:**
- Create: `crates/onesync-protocol/tests/schema_compliance.rs`

- [ ] **Step 10.1: Write the failing test**

Create `crates/onesync-protocol/tests/schema_compliance.rs`:

```rust
//! Validates that the Rust types round-trip through serde_json and conform to
//! the canonical-types JSON Schema sidecar.

use jsonschema::JSONSchema;
use onesync_protocol::*;

const SCHEMA_PATH: &str = "../../docs/spec/canonical-types.schema.json";

fn schema() -> JSONSchema {
    let raw = std::fs::read_to_string(SCHEMA_PATH)
        .unwrap_or_else(|e| panic!("read {SCHEMA_PATH}: {e}"));
    let value: serde_json::Value = serde_json::from_str(&raw).expect("schema parses");
    JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .compile(&value)
        .expect("schema compiles")
}

fn validate_against(schema: &JSONSchema, sub_def: &str, instance: &serde_json::Value) {
    // jsonschema can't directly validate a $def; wrap in a one-shot $ref.
    let wrapper: serde_json::Value = serde_json::json!({
        "$ref": format!("#/$defs/{sub_def}")
    });
    let base: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(SCHEMA_PATH).unwrap()
    ).unwrap();
    let mut combined = base.clone();
    combined["$ref"] = wrapper["$ref"].clone();
    let compiled = JSONSchema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .compile(&combined)
        .expect("schema compiles");
    let result = compiled.validate(instance);
    if let Err(errors) = result {
        let messages: Vec<_> = errors.map(|e| format!("- {e}")).collect();
        panic!("{sub_def} failed schema validation:\n{}", messages.join("\n"));
    }
}

#[test]
fn account_validates() {
    let s = schema();
    let _ = s; // keep `schema()` exercised in case the wrapper path changes.
    let acct = serde_json::json!({
        "id": "acct_01J8X7CFGMZG7Y4DC0VA8DZW2H",
        "kind": "personal",
        "upn": "alice@example.com",
        "tenant_id": "9188040d-6c67-4c5b-b112-36a304b66dad",
        "drive_id": "drv-1",
        "display_name": "Alice",
        "keychain_ref": "kc-1",
        "scopes": ["Files.ReadWrite"],
        "created_at": "2026-05-11T10:00:00Z",
        "updated_at": "2026-05-11T10:00:00Z"
    });
    validate_against(&schema(), "Account", &acct);
    let _: account::Account = serde_json::from_value(acct).expect("rust round-trip");
}

#[test]
fn pair_validates() {
    let pair = serde_json::json!({
        "id": "pair_01J8X7CFGMZG7Y4DC0VA8DZW2H",
        "account_id": "acct_01J8X7CFGMZG7Y4DC0VA8DZW2H",
        "local_path": "/Users/alice/OneDrive",
        "remote_item_id": "drive-item-root",
        "remote_path": "/",
        "display_name": "OneDrive",
        "status": "active",
        "paused": false,
        "created_at": "2026-05-11T10:00:00Z",
        "updated_at": "2026-05-11T10:00:00Z",
        "conflict_count": 0
    });
    validate_against(&schema(), "Pair", &pair);
    let _: pair::Pair = serde_json::from_value(pair).expect("rust round-trip");
}

#[test]
fn file_entry_validates() {
    let entry = serde_json::json!({
        "pair_id": "pair_01J8X7CFGMZG7Y4DC0VA8DZW2H",
        "relative_path": "Documents/notes.md",
        "kind": "file",
        "sync_state": "clean",
        "updated_at": "2026-05-11T10:00:00Z"
    });
    validate_against(&schema(), "FileEntry", &entry);
    let _: file_entry::FileEntry = serde_json::from_value(entry).expect("rust round-trip");
}

#[test]
fn file_op_id_with_two_char_prefix_validates() {
    // Regression: the Id base pattern previously rejected `op_` (2-char prefix).
    let op_id = serde_json::json!("op_01J8X7CFGMZG7Y4DC0VA8DZW2H");
    validate_against(&schema(), "FileOpId", &op_id);
}

#[test]
fn instance_config_validates() {
    let cfg = serde_json::json!({
        "log_level": "info",
        "notify": true,
        "allow_metered": false,
        "min_free_gib": 2,
        "updated_at": "2026-05-11T10:00:00Z"
    });
    validate_against(&schema(), "InstanceConfig", &cfg);
    let _: config::InstanceConfig = serde_json::from_value(cfg).expect("rust round-trip");
}

#[test]
fn error_envelope_validates() {
    let err = serde_json::json!({
        "kind": "graph.throttled",
        "message": "retry after 30s",
        "retryable": true
    });
    validate_against(&schema(), "ErrorEnvelope", &err);
    let _: errors::ErrorEnvelope = serde_json::from_value(err).expect("rust round-trip");
}
```

- [ ] **Step 10.2: Run, watch the suite go green**

Run: `cargo nextest run -p onesync-protocol --test schema_compliance`
Expected: 6 tests passed.

If any fail, the failure message names which `$def` diverged from the Rust shape. Fix in `crates/onesync-protocol/src/` and re-run.

- [ ] **Step 10.3: Lint and commit**

```
cargo clippy -p onesync-protocol --all-targets -- -D warnings
cargo fmt --all -- --check
jj commit -m "test(protocol): JSON Schema compliance for canonical types

Co-Authored-By: <name> <email>"
```

---

## Task 11: `onesync-core` crate skeleton and limits module

**Files:**
- Create: `crates/onesync-core/Cargo.toml`
- Create: `crates/onesync-core/src/lib.rs`
- Create: `crates/onesync-core/src/limits.rs`

- [ ] **Step 11.1: Cargo manifest**

```toml
[package]
name = "onesync-core"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
onesync-protocol = { path = "../onesync-protocol" }
async-trait      = { workspace = true }
serde            = { workspace = true }
thiserror        = { workspace = true }

[dev-dependencies]
proptest = { workspace = true }
```

- [ ] **Step 11.2: `lib.rs`**

```rust
//! Pure-logic core for onesync.
//!
//! Hosts the engine, the conflict policy, and the port traits. Has no I/O
//! dependencies. See [`docs/spec/02-architecture.md`](../../../../docs/spec/02-architecture.md).

#![forbid(unsafe_code)]

pub mod limits;
pub mod ports;
```

- [ ] **Step 11.3: Write the failing limits test**

Create `crates/onesync-core/src/limits.rs` containing only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_have_documented_values() {
        // Cross-check a representative sample against docs/spec/09-development-guidelines.md.
        assert_eq!(MAX_PAIRS_PER_INSTANCE, 16);
        assert_eq!(MAX_QUEUE_DEPTH_PER_PAIR, 4096);
        assert_eq!(MAX_FILE_SIZE_BYTES, 50 * GIB);
        assert_eq!(GRAPH_SMALL_UPLOAD_MAX_BYTES, 4 * MIB);
        assert_eq!(SESSION_CHUNK_BYTES % (320 * KIB), 0);
        assert_eq!(IPC_FRAME_MAX_BYTES, MIB);
        assert_eq!(AUDIT_RETENTION_DAYS, 30);
    }

    #[test]
    fn unit_suffix_constants() {
        assert_eq!(KIB, 1024);
        assert_eq!(MIB, 1024 * 1024);
        assert_eq!(GIB, 1024 * 1024 * 1024);
    }
}
```

Run: `cargo nextest run -p onesync-core limits::tests`
Expected: compile error — symbols undefined.

- [ ] **Step 11.4: Implement the limits**

Prepend to `crates/onesync-core/src/limits.rs`:

```rust
//! Compile-time limits.
//!
//! Every limit in [`docs/spec/09-development-guidelines.md`](../../../../docs/spec/09-development-guidelines.md)
//! lives here. Units are part of the identifier; values are immediate. Operator-tunable
//! values live in `onesync_protocol::config::InstanceConfig`, never in this module.

#![allow(clippy::doc_markdown)]

pub const KIB: u64 = 1024;
pub const MIB: u64 = 1024 * KIB;
pub const GIB: u64 = 1024 * MIB;

// --- Sync engine ---
pub const MAX_PAIRS_PER_INSTANCE: usize           = 16;
pub const MAX_QUEUE_DEPTH_PER_PAIR: usize         = 4_096;
pub const MAX_CONCURRENT_TRANSFERS: usize         = 4;
pub const PAIR_CONCURRENT_TRANSFERS: usize        = 2;
pub const RETRY_MAX_ATTEMPTS: u32                 = 5;
pub const RETRY_BACKOFF_BASE_MS: u64              = 1_000;
pub const DELTA_POLL_INTERVAL_MS: u64             = 30_000;
pub const LOCAL_DEBOUNCE_MS: u64                  = 500;
pub const REMOTE_DEBOUNCE_MS: u64                 = 2_000;
pub const CYCLE_PHASE_TIMEOUT_MS: u64             = 60_000;
pub const CONFLICT_MTIME_TOLERANCE_MS: u64        = 1_000;
pub const CONFLICT_RENAME_RETRIES: u32            = 8;

// --- Filesystem ---
pub const MAX_FILE_SIZE_BYTES: u64                = 50 * GIB;
pub const MAX_PATH_BYTES: usize                   = 1_024;
pub const HASH_BLOCK_BYTES: usize                 = MIB as usize;
pub const READ_INLINE_MAX: usize                  = 64 * KIB as usize;
pub const FSEVENT_BUFFER_DEPTH: usize             = 4_096;
pub const SCAN_QUEUE_DEPTH_MAX: usize             = 65_536;
pub const SCAN_INFLIGHT_MAX: usize                = 1_024;
pub const DISK_FREE_MARGIN_BYTES: u64             = 2 * GIB;

// --- Microsoft Graph ---
pub const GRAPH_SMALL_UPLOAD_MAX_BYTES: u64       = 4 * MIB;
pub const SESSION_CHUNK_BYTES: u64                = 10 * MIB;
pub const GRAPH_RPS_PER_ACCOUNT: u32              = 8;
pub const TOKEN_REFRESH_LEEWAY_S: u64             = 120;
pub const AUTH_LISTENER_TIMEOUT_S: u64            = 300;

// --- State store ---
pub const STATE_POOL_SIZE: usize                  = 4;
pub const AUDIT_RETENTION_DAYS: u32               = 30;
pub const RUN_HISTORY_RETENTION_DAYS: u32         = 90;
pub const CONFLICT_RETENTION_DAYS: u32            = 180;
pub const LOG_ROTATE_BYTES: u64                   = 32 * MIB;
pub const LOG_RETAIN_FILES: u32                   = 10;

// --- IPC and lifecycle ---
pub const IPC_FRAME_MAX_BYTES: u64                = MIB;
pub const IPC_KEEPALIVE_MS: u64                   = 30_000;
pub const SUB_GC_INTERVAL_MS: u64                 = 60_000;
pub const INSTALL_TIMEOUT_S: u64                  = 60;
pub const SHUTDOWN_DRAIN_TIMEOUT_S: u64           = 30;
pub const UPGRADE_DRAIN_TIMEOUT_S: u64            = 30;
pub const MAX_CLOCK_SKEW_S: i64                   = 600;
```

(`MAX_RUNTIME_WORKERS` is a runtime-computed value, not a `const`. It will be exposed as a function in `onesync-daemon` later; mention this in the doc when the time comes.)

- [ ] **Step 11.5: Run tests, lint, commit**

```
cargo nextest run -p onesync-core
cargo clippy -p onesync-core --all-targets -- -D warnings
cargo fmt --all -- --check
jj commit -m "feat(core): limits module with every named constant from the spec

Co-Authored-By: <name> <email>"
```

---

## Task 12: Port traits

**Files:**
- Create: `crates/onesync-core/src/ports/mod.rs`
- Create: `crates/onesync-core/src/ports/{state,local_fs,remote_drive,token_vault,clock,id_generator,audit_sink}.rs`

These are trait declarations only — no implementations. The point is to fix the *signatures* the rest of the project will program against.

- [ ] **Step 12.1: `ports/mod.rs`**

```rust
//! Port traits and their port-level error types.

pub mod audit_sink;
pub mod clock;
pub mod id_generator;
pub mod local_fs;
pub mod remote_drive;
pub mod state;
pub mod token_vault;

pub use audit_sink::AuditSink;
pub use clock::Clock;
pub use id_generator::IdGenerator;
pub use local_fs::{LocalFs, LocalFsError};
pub use remote_drive::{RemoteDrive, GraphError};
pub use state::{StateStore, StateError};
pub use token_vault::{TokenVault, VaultError};
```

- [ ] **Step 12.2: `clock.rs`**

```rust
use onesync_protocol::primitives::Timestamp;

pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}
```

- [ ] **Step 12.3: `id_generator.rs`**

```rust
use onesync_protocol::id::{Id, IdPrefix};

pub trait IdGenerator: Send + Sync {
    fn new_id<T: IdPrefix + 'static>(&self) -> Id<T>;
}
```

- [ ] **Step 12.4: `audit_sink.rs`**

```rust
use onesync_protocol::audit::AuditEvent;

pub trait AuditSink: Send + Sync {
    fn emit(&self, event: AuditEvent);
}
```

- [ ] **Step 12.5: `state.rs`**

```rust
use async_trait::async_trait;
use onesync_protocol::{
    account::Account,
    audit::AuditEvent,
    conflict::Conflict,
    file_entry::FileEntry,
    file_op::FileOp,
    id::{AccountId, FileOpId, PairId},
    pair::Pair,
    path::RelPath,
    sync_run::SyncRun,
};

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("backend i/o: {0}")]
    Io(String),
    #[error("schema mismatch: {0}")]
    Schema(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("constraint violated: {0}")]
    Constraint(String),
}

#[async_trait]
pub trait StateStore: Send + Sync {
    async fn account_upsert(&self, account: &Account) -> Result<(), StateError>;
    async fn account_get(&self, id: &AccountId) -> Result<Option<Account>, StateError>;
    async fn pair_upsert(&self, pair: &Pair) -> Result<(), StateError>;
    async fn pair_get(&self, id: &PairId) -> Result<Option<Pair>, StateError>;
    async fn pairs_active(&self) -> Result<Vec<Pair>, StateError>;
    async fn file_entry_upsert(&self, entry: &FileEntry) -> Result<(), StateError>;
    async fn file_entry_get(
        &self,
        pair: &PairId,
        path: &RelPath,
    ) -> Result<Option<FileEntry>, StateError>;
    async fn file_entries_dirty(
        &self,
        pair: &PairId,
        limit: usize,
    ) -> Result<Vec<FileEntry>, StateError>;
    async fn run_record(&self, run: &SyncRun) -> Result<(), StateError>;
    async fn op_insert(&self, op: &FileOp) -> Result<(), StateError>;
    async fn op_update_status(
        &self,
        id: &FileOpId,
        status: onesync_protocol::enums::FileOpStatus,
    ) -> Result<(), StateError>;
    async fn conflict_insert(&self, c: &Conflict) -> Result<(), StateError>;
    async fn conflicts_unresolved(&self, pair: &PairId) -> Result<Vec<Conflict>, StateError>;
    async fn audit_append(&self, evt: &AuditEvent) -> Result<(), StateError>;
}
```

- [ ] **Step 12.6: `local_fs.rs`**

```rust
use async_trait::async_trait;
use onesync_protocol::{file_side::FileSide, path::AbsPath, primitives::ContentHash};

#[derive(Debug, thiserror::Error)]
pub enum LocalFsError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("not mounted: {0}")]
    NotMounted(String),
    #[error("disk full")]
    DiskFull,
    #[error("quota exceeded")]
    QuotaExceeded,
    #[error("already running: {0}")]
    AlreadyRunning(String),
    #[error("cross-volume rename ({method})")]
    CrossVolumeRename { method: &'static str },
    #[error("invalid path: {reason}")]
    InvalidPath { reason: String },
    #[error("raced (mtime changed under us)")]
    Raced,
    #[error("io: {0}")]
    Io(String),
}

/// Placeholder stream types; real ones land in `onesync-fs-local` (M2).
pub struct LocalEventStream;
pub struct LocalScanStream;
pub struct LocalReadStream;
pub struct LocalWriteStream;

#[async_trait]
pub trait LocalFs: Send + Sync {
    async fn scan(&self, root: &AbsPath) -> Result<LocalScanStream, LocalFsError>;
    async fn read(&self, path: &AbsPath) -> Result<LocalReadStream, LocalFsError>;
    async fn write_atomic(
        &self,
        path: &AbsPath,
        stream: LocalWriteStream,
    ) -> Result<FileSide, LocalFsError>;
    async fn rename(&self, from: &AbsPath, to: &AbsPath) -> Result<(), LocalFsError>;
    async fn delete(&self, path: &AbsPath) -> Result<(), LocalFsError>;
    async fn mkdir_p(&self, path: &AbsPath) -> Result<(), LocalFsError>;
    async fn watch(&self, root: &AbsPath) -> Result<LocalEventStream, LocalFsError>;
    async fn hash(&self, path: &AbsPath) -> Result<ContentHash, LocalFsError>;
}
```

- [ ] **Step 12.7: `remote_drive.rs`**

```rust
use async_trait::async_trait;
use onesync_protocol::{
    account::Account,
    primitives::{DeltaCursor, DriveId, DriveItemId},
};

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("re-authentication required")]
    ReAuthRequired,
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("name conflict")]
    NameConflict,
    #[error("resync required")]
    ResyncRequired,
    #[error("stale (server etag {server_etag})")]
    Stale { server_etag: String },
    #[error("invalid range")]
    InvalidRange,
    #[error("throttled (retry after {retry_after_s}s)")]
    Throttled { retry_after_s: u64 },
    #[error("transient: {0}")]
    Transient(String),
    #[error("network: {0}")]
    Network { source: String },
    #[error("decode: {0}")]
    Decode { detail: String },
    #[error("hash mismatch")]
    HashMismatch,
    #[error("file too large")]
    TooLarge,
}

/// Placeholder shapes; real ones land in `onesync-graph` (M3).
pub struct AccessToken;
pub struct AccountProfile;
pub struct RemoteItem;
pub struct RemoteItemId;
pub struct DeltaPage;
pub struct RemoteReadStream;
pub struct UploadSession;

#[async_trait]
pub trait RemoteDrive: Send + Sync {
    async fn account_profile(
        &self,
        token: &AccessToken,
    ) -> Result<AccountProfile, GraphError>;
    async fn item_by_path(
        &self,
        drive: &DriveId,
        path: &str,
    ) -> Result<Option<RemoteItem>, GraphError>;
    async fn delta(
        &self,
        drive: &DriveId,
        cursor: Option<&DeltaCursor>,
    ) -> Result<DeltaPage, GraphError>;
    async fn download(
        &self,
        item: &RemoteItemId,
    ) -> Result<RemoteReadStream, GraphError>;
    async fn upload_small(
        &self,
        parent: &RemoteItemId,
        name: &str,
        bytes: &[u8],
    ) -> Result<RemoteItem, GraphError>;
    async fn upload_session(
        &self,
        parent: &RemoteItemId,
        name: &str,
        size: u64,
    ) -> Result<UploadSession, GraphError>;
    async fn rename(
        &self,
        item: &RemoteItemId,
        new_name: &str,
    ) -> Result<RemoteItem, GraphError>;
    async fn delete(&self, item: &RemoteItemId) -> Result<(), GraphError>;
    async fn mkdir(
        &self,
        parent: &RemoteItemId,
        name: &str,
    ) -> Result<RemoteItem, GraphError>;
}

// Silence the unused-import lint until M3 fleshes these out.
#[allow(dead_code)]
fn _types_kept_in_scope(_a: &Account) {}
```

- [ ] **Step 12.8: `token_vault.rs`**

```rust
use async_trait::async_trait;
use onesync_protocol::{id::AccountId, primitives::KeychainRef};

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("not found")]
    NotFound,
    #[error("backend: {0}")]
    Backend(String),
}

pub struct RefreshToken(pub String);

#[async_trait]
pub trait TokenVault: Send + Sync {
    async fn store_refresh(
        &self,
        account: &AccountId,
        token: &RefreshToken,
    ) -> Result<KeychainRef, VaultError>;
    async fn load_refresh(&self, account: &AccountId)
        -> Result<RefreshToken, VaultError>;
    async fn delete(&self, account: &AccountId) -> Result<(), VaultError>;
}
```

- [ ] **Step 12.9: Build, lint, commit**

```
cargo check -p onesync-core
cargo clippy -p onesync-core --all-targets -- -D warnings
cargo fmt --all -- --check
jj commit -m "feat(core): port traits and port-level error enums

Co-Authored-By: <name> <email>"
```

---

## Task 13: `onesync-time` crate with `SystemClock` and `UlidGenerator`

**Files:**
- Create: `crates/onesync-time/Cargo.toml`
- Create: `crates/onesync-time/src/lib.rs`
- Create: `crates/onesync-time/src/system_clock.rs`
- Create: `crates/onesync-time/src/ulid_generator.rs`

- [ ] **Step 13.1: Cargo manifest**

```toml
[package]
name = "onesync-time"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[lints]
workspace = true

[dependencies]
onesync-core     = { path = "../onesync-core" }
onesync-protocol = { path = "../onesync-protocol" }
chrono           = { workspace = true }
ulid             = { workspace = true }

[dev-dependencies]
proptest = { workspace = true }
```

- [ ] **Step 13.2: `lib.rs`**

```rust
//! Concrete `Clock` and `IdGenerator` adapters.

#![forbid(unsafe_code)]

pub mod fakes;
pub mod system_clock;
pub mod ulid_generator;

pub use system_clock::SystemClock;
pub use ulid_generator::UlidGenerator;
```

- [ ] **Step 13.3: `system_clock.rs`**

```rust
use chrono::Utc;
use onesync_core::ports::Clock;
use onesync_protocol::primitives::Timestamp;

#[derive(Default, Debug)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_datetime(Utc::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Utc};

    #[test]
    fn system_clock_returns_a_recent_timestamp() {
        let clock = SystemClock;
        let now = clock.now().into_inner();
        let real = Utc::now();
        let delta = (real - now).num_seconds().abs();
        assert!(delta < 5, "system clock drift was {delta}s");
        assert!(now.year() >= 2026, "year was {}", now.year());
    }
}
```

- [ ] **Step 13.4: `ulid_generator.rs`**

```rust
use std::marker::PhantomData;

use onesync_core::ports::IdGenerator;
use onesync_protocol::id::{Id, IdPrefix};
use ulid::Ulid;

#[derive(Default, Debug)]
pub struct UlidGenerator {
    _marker: PhantomData<()>,
}

impl IdGenerator for UlidGenerator {
    fn new_id<T: IdPrefix + 'static>(&self) -> Id<T> {
        Id::from_ulid(Ulid::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use onesync_protocol::id::AccountTag;

    #[test]
    fn ulid_generator_produces_distinct_ids() {
        let gen = UlidGenerator::default();
        let a: Id<AccountTag> = gen.new_id();
        let b: Id<AccountTag> = gen.new_id();
        assert_ne!(a, b);
    }
}
```

- [ ] **Step 13.5: Run, lint, commit**

```
cargo nextest run -p onesync-time
cargo clippy -p onesync-time --all-targets -- -D warnings
cargo fmt --all -- --check
jj commit -m "feat(time): SystemClock and UlidGenerator adapters

Co-Authored-By: <name> <email>"
```

---

## Task 14: `onesync-time::fakes` — `TestClock` and `TestIdGenerator`

**Files:**
- Create: `crates/onesync-time/src/fakes.rs`

- [ ] **Step 14.1: Write failing tests for `TestClock`**

Create `crates/onesync-time/src/fakes.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use onesync_protocol::id::PairTag;
    use std::time::Duration;

    #[test]
    fn test_clock_returns_the_set_time() {
        let clock = TestClock::at(chrono::Utc.with_ymd_and_hms(2026, 5, 11, 10, 0, 0).unwrap());
        let t1 = clock.now().into_inner();
        let t2 = clock.now().into_inner();
        assert_eq!(t1, t2);
    }

    #[test]
    fn test_clock_advance_moves_forward() {
        let clock = TestClock::at(chrono::Utc.with_ymd_and_hms(2026, 5, 11, 10, 0, 0).unwrap());
        let before = clock.now().into_inner();
        clock.advance(Duration::from_secs(60));
        let after = clock.now().into_inner();
        assert_eq!((after - before).num_seconds(), 60);
    }

    #[test]
    fn test_id_generator_is_deterministic() {
        let gen = TestIdGenerator::seeded(42);
        let a: Id<PairTag> = gen.new_id();
        let b: Id<PairTag> = gen.new_id();
        assert_ne!(a, b);

        let gen2 = TestIdGenerator::seeded(42);
        let c: Id<PairTag> = gen2.new_id();
        assert_eq!(a, c, "same seed must produce same first ID");
    }
}
```

- [ ] **Step 14.2: Implement the fakes**

Prepend:

```rust
//! Test doubles for the `Clock` and `IdGenerator` ports.

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use onesync_core::ports::{Clock, IdGenerator};
use onesync_protocol::{id::{Id, IdPrefix}, primitives::Timestamp};
use ulid::Ulid;

#[derive(Debug)]
pub struct TestClock {
    inner: Mutex<DateTime<Utc>>,
}

impl TestClock {
    #[must_use]
    pub fn at(t: DateTime<Utc>) -> Self {
        Self { inner: Mutex::new(t) }
    }

    pub fn advance(&self, d: Duration) {
        let mut guard = self.inner.lock().expect("test clock mutex poisoned");
        *guard += chrono::Duration::from_std(d).expect("duration fits chrono");
    }

    pub fn set(&self, t: DateTime<Utc>) {
        *self.inner.lock().expect("test clock mutex poisoned") = t;
    }
}

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        let t = *self.inner.lock().expect("test clock mutex poisoned");
        Timestamp::from_datetime(t)
    }
}

#[derive(Debug)]
pub struct TestIdGenerator {
    counter: Mutex<u64>,
    seed: u64,
}

impl TestIdGenerator {
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        Self { counter: Mutex::new(0), seed }
    }
}

impl IdGenerator for TestIdGenerator {
    fn new_id<T: IdPrefix + 'static>(&self) -> Id<T> {
        let mut guard = self.counter.lock().expect("test id-gen mutex poisoned");
        *guard += 1;
        let n = *guard;
        // Pack seed and counter into a deterministic 128-bit value.
        let bits = (u128::from(self.seed) << 64) | u128::from(n);
        Id::from_ulid(Ulid::from(bits))
    }
}
```

- [ ] **Step 14.3: Run tests, lint, commit**

```
cargo nextest run -p onesync-time
cargo clippy -p onesync-time --all-targets -- -D warnings
cargo fmt --all -- --check
jj commit -m "feat(time): TestClock and TestIdGenerator fakes

Co-Authored-By: <name> <email>"
```

---

## Task 15: Workspace-wide gate + README build commands

**Files:**
- Create: `README.md`

- [ ] **Step 15.1: Run the full workspace gate**

```
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo nextest run --workspace
```

All three must exit 0. Fix anything that drops out before moving on.

- [ ] **Step 15.2: Write a minimal `README.md`**

```markdown
# onesync

macOS background daemon and CLI for two-way synchronisation between a designated local folder
and a designated folder in OneDrive (Personal or Business). Written in Safe Rust.

Design: [`docs/spec/`](docs/spec/). Roadmap: [`docs/plans/2026-05-11-roadmap.md`](docs/plans/2026-05-11-roadmap.md).

## Build

Requires Rust 1.95.0 (pinned via `rust-toolchain.toml`) and `cargo-nextest`.

```sh
cargo build --workspace
cargo nextest run --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

## Status

M1 — Foundations (in progress).
```

- [ ] **Step 15.3: Commit the milestone close**

```
jj commit -m "docs: README with build commands for M1 foundations

Co-Authored-By: <name> <email>"
```

- [ ] **Step 15.4: Push**

```
jj git push --bookmark main
```

The bookmark `main` already tracks `origin`. Expected output: `Add commits to main on origin`.

---

## Self-review checklist (run after the last task)

- [ ] Every limit named in [`docs/spec/09-development-guidelines.md`](../spec/09-development-guidelines.md) has a `pub const` in `crates/onesync-core/src/limits.rs`.
- [ ] Every entity with a `$def` in [`canonical-types.schema.json`](../spec/canonical-types.schema.json) has a Rust type in `crates/onesync-protocol/src/`.
- [ ] Schema-compliance test covers `Account`, `Pair`, `FileEntry`, `FileOpId`, `InstanceConfig`, `ErrorEnvelope`.
- [ ] No `unwrap()`, `expect()`, `panic!()`, `todo!()`, `unimplemented!()` in `src/` (test code excluded; `expect()` on test mutexes is acceptable).
- [ ] `cargo nextest run --workspace` is green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is green.
- [ ] `cargo fmt --all -- --check` is green.
- [ ] M1 commits visible on `origin/main`.

If any check fails, fix in place — do not declare M1 done.

---

## Exit and handoff

When the self-review is green:

- Update [`2026-05-11-roadmap.md`](2026-05-11-roadmap.md) M1 row to `Complete` with the merge commit SHA.
- Open the M2 plan: `docs/plans/2026-MM-DD-m2-state-and-local-fs.md`. Author it with the same writing-plans skill before any M2 task starts.
- Do **not** start M2 implementation tasks before that plan exists. Planning before coding is the project's contract.
