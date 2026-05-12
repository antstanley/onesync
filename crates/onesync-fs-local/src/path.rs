//! Runtime path-canonicalisation helpers.
//!
//! The `RelPath` and `AbsPath` newtypes already enforce static validation. These helpers
//! work with paths as observed from the filesystem at runtime.

use std::path::{Path, PathBuf};

use onesync_protocol::path::{AbsPath, RelPath};

use crate::error::LocalFsAdapterError;

/// Resolve `path` against `root`, returning a relative path within the pair root.
///
/// # Errors
/// Returns `InvalidPath` if `path` is not under `root` or if the resulting relative
/// path fails `RelPath` validation (e.g. contains `..` after stripping).
pub fn relativise(root: &AbsPath, path: &AbsPath) -> Result<RelPath, LocalFsAdapterError> {
    let root_pb = PathBuf::from(root.as_str());
    let path_pb = PathBuf::from(path.as_str());
    let rel = path_pb
        .strip_prefix(&root_pb)
        .map_err(|_| LocalFsAdapterError::InvalidPath {
            reason: format!("{} is not under {}", path.as_str(), root.as_str()),
        })?;
    let rel_str = rel
        .to_str()
        .ok_or_else(|| LocalFsAdapterError::InvalidPath {
            reason: "non-UTF8 path component".into(),
        })?;
    // Empty (path == root) means relativising the root itself; reject — caller should not
    // ask for a RelPath in this case.
    if rel_str.is_empty() {
        return Err(LocalFsAdapterError::InvalidPath {
            reason: "path equals the pair root".into(),
        });
    }
    rel_str
        .parse::<RelPath>()
        .map_err(|e| LocalFsAdapterError::InvalidPath {
            reason: format!("relpath validation: {e}"),
        })
}

/// Compose an absolute path from a pair root and a relative path.
#[allow(clippy::expect_used)]
// LINT: the prefix is already an `AbsPath`; appending a validated `RelPath` cannot produce
//       a malformed result. Returning Result here would force every caller into noise.
#[must_use]
pub fn absolutise(root: &AbsPath, rel: &RelPath) -> AbsPath {
    let mut s = root.as_str().to_owned();
    if !s.ends_with('/') {
        s.push('/');
    }
    s.push_str(rel.as_str());
    s.parse::<AbsPath>()
        .expect("composed path is absolute and validated; the prefix is already an AbsPath")
}

/// Determine whether two paths live on the same filesystem volume.
///
/// On macOS we use `metadata().dev()` to compare device numbers. If either `metadata()`
/// call fails (path doesn't exist yet), returns `false` — the safer fallback that may
/// trigger the copy+delete branch unnecessarily, but never the rename branch on a
/// cross-volume target.
#[must_use]
pub fn same_volume(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    ma.dev() == mb.dev()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn abs(s: &str) -> AbsPath {
        s.parse::<AbsPath>().expect("absolutepath")
    }

    #[test]
    fn relativise_under_root_succeeds() {
        let root = abs("/Users/alice/OneDrive");
        let path = abs("/Users/alice/OneDrive/Documents/notes.md");
        let rel = relativise(&root, &path).expect("relativise");
        assert_eq!(rel.as_str(), "Documents/notes.md");
    }

    #[test]
    fn relativise_outside_root_fails() {
        let root = abs("/Users/alice/OneDrive");
        let path = abs("/Users/alice/Desktop/notes.md");
        let err = relativise(&root, &path).expect_err("rejected");
        assert!(matches!(err, LocalFsAdapterError::InvalidPath { .. }));
    }

    #[test]
    fn relativise_path_equals_root_fails() {
        let root = abs("/Users/alice/OneDrive");
        let path = abs("/Users/alice/OneDrive");
        let err = relativise(&root, &path).expect_err("rejected");
        assert!(matches!(err, LocalFsAdapterError::InvalidPath { .. }));
    }

    #[test]
    fn absolutise_concatenates_and_validates() {
        let root = abs("/Users/alice/OneDrive");
        let rel: RelPath = "Documents/notes.md".parse().unwrap();
        let abs_back = absolutise(&root, &rel);
        assert_eq!(
            abs_back.as_str(),
            "/Users/alice/OneDrive/Documents/notes.md"
        );
    }

    #[test]
    fn absolutise_handles_trailing_slash_on_root() {
        let root = abs("/Users/alice/OneDrive/");
        let rel: RelPath = "Documents/notes.md".parse().unwrap();
        let abs_back = absolutise(&root, &rel);
        assert_eq!(
            abs_back.as_str(),
            "/Users/alice/OneDrive/Documents/notes.md"
        );
    }

    #[test]
    fn same_volume_within_tempdir_is_true() {
        let tmp = TempDir::new().expect("tmpdir");
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::File::create(&a).expect("create a");
        std::fs::File::create(&b).expect("create b");
        assert!(same_volume(&a, &b));
    }

    #[test]
    fn same_volume_with_nonexistent_returns_false() {
        let nonexistent_a = Path::new("/this/should/not/exist/a");
        let nonexistent_b = Path::new("/this/should/not/exist/b");
        assert!(!same_volume(nonexistent_a, nonexistent_b));
    }
}
