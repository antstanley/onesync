//! Rename, delete, and `mkdir -p` operations.

use std::path::Path;

use crate::error::LocalFsAdapterError;
use crate::path::same_volume;

/// Rename `from` to `to`. Degrades to copy+delete if the paths are on different volumes.
///
/// # Errors
/// On the same volume, returns `LocalFsAdapterError::Io` for the underlying `rename(2)`
/// failure. On different volumes, returns `LocalFsAdapterError::CrossVolumeRename` even
/// on success — the caller treats this as an audit signal, not a fatal error, after
/// confirming the destination exists.
pub fn rename(from: &Path, to: &Path) -> Result<(), LocalFsAdapterError> {
    if from.exists()
        && to.parent().is_some_and(Path::exists)
        && !same_volume(from, to.parent().unwrap_or(from))
    {
        // Cross-volume: copy + delete, then surface the degradation.
        std::fs::copy(from, to)?;
        std::fs::remove_file(from)?;
        return Err(LocalFsAdapterError::CrossVolumeRename {
            method: "copy+delete",
        });
    }
    std::fs::rename(from, to).map_err(LocalFsAdapterError::from)
}

/// Delete a file or empty directory.
///
/// # Errors
/// Returns `LocalFsAdapterError::Io` for the underlying failure (not found, permission
/// denied, non-empty directory).
pub fn delete(path: &Path) -> Result<(), LocalFsAdapterError> {
    let meta = std::fs::metadata(path)?;
    if meta.is_dir() {
        std::fs::remove_dir(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Create `path` and any missing parent directories.
///
/// # Errors
/// Returns `LocalFsAdapterError::Io` if creation fails for reasons other than
/// "already exists as a directory".
pub fn mkdir_p(path: &Path) -> Result<(), LocalFsAdapterError> {
    std::fs::create_dir_all(path).map_err(LocalFsAdapterError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str, content: &[u8]) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).expect("create");
        f.write_all(content).expect("write");
        p
    }

    #[test]
    fn rename_within_same_volume_succeeds() {
        let tmp = TempDir::new().expect("tmpdir");
        let a = write_file(tmp.path(), "a.txt", b"hello");
        let b = tmp.path().join("b.txt");
        rename(&a, &b).expect("rename");
        assert!(!a.exists());
        assert!(b.exists());
        assert_eq!(std::fs::read(&b).unwrap(), b"hello");
    }

    #[test]
    fn delete_file_succeeds() {
        let tmp = TempDir::new().expect("tmpdir");
        let p = write_file(tmp.path(), "doomed.txt", b"bye");
        delete(&p).expect("delete");
        assert!(!p.exists());
    }

    #[test]
    fn delete_empty_directory_succeeds() {
        let tmp = TempDir::new().expect("tmpdir");
        let d = tmp.path().join("emptydir");
        std::fs::create_dir(&d).expect("mkdir");
        delete(&d).expect("delete");
        assert!(!d.exists());
    }

    #[test]
    fn delete_non_empty_directory_fails() {
        let tmp = TempDir::new().expect("tmpdir");
        let d = tmp.path().join("nonempty");
        std::fs::create_dir(&d).expect("mkdir");
        write_file(&d, "inside.txt", b"present");
        let err = delete(&d).expect_err("should fail on non-empty dir");
        assert!(matches!(err, LocalFsAdapterError::Io(_)));
    }

    #[test]
    fn mkdir_p_creates_nested_directories() {
        let tmp = TempDir::new().expect("tmpdir");
        let nested = tmp.path().join("a").join("b").join("c");
        mkdir_p(&nested).expect("mkdir_p");
        assert!(nested.is_dir());
    }

    #[test]
    fn mkdir_p_is_idempotent() {
        let tmp = TempDir::new().expect("tmpdir");
        let d = tmp.path().join("dir");
        mkdir_p(&d).expect("first");
        mkdir_p(&d).expect("second (should be no-op)");
        assert!(d.is_dir());
    }

    #[test]
    fn delete_nonexistent_path_fails() {
        let p = std::path::PathBuf::from("/this/should/not/exist/anywhere");
        let err = delete(&p).expect_err("not found");
        assert!(matches!(err, LocalFsAdapterError::Io(_)));
    }
}
