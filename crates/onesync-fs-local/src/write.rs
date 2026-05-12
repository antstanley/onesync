//! Atomic file writes (temp + fsync + rename + dir-fsync).

use std::io::Write;
use std::path::Path;

use onesync_core::limits::HASH_BLOCK_BYTES;
use onesync_protocol::{
    enums::FileKind,
    file_side::FileSide,
    primitives::{ContentHash, Timestamp},
};

use crate::error::LocalFsAdapterError;

/// Write `bytes` to `target` atomically: temp file in the same directory, fsync the
/// temp file, rename into place, fsync the directory.
///
/// Returns the `FileSide` describing the resulting file (kind=File, size, BLAKE3
/// content hash, mtime, `etag=None`, `remote_item_id=None`).
///
/// # Errors
/// Returns `LocalFsAdapterError::InvalidPath` if the target has no parent directory.
/// Returns `LocalFsAdapterError::Io` for any I/O failure (open, write, fsync, rename).
pub fn write_atomic(target: &Path, bytes: &[u8]) -> Result<FileSide, LocalFsAdapterError> {
    let parent = target
        .parent()
        .ok_or_else(|| LocalFsAdapterError::InvalidPath {
            reason: "target has no parent".into(),
        })?;

    // 1. Create the temp file in the same directory so the rename stays on-volume.
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;

    // 2. Stream bytes into the temp file while hashing.
    let mut hasher = blake3::Hasher::new();
    let mut total: u64 = 0;
    for chunk in bytes.chunks(HASH_BLOCK_BYTES) {
        tmp.write_all(chunk)?;
        hasher.update(chunk);
        #[allow(clippy::cast_possible_truncation)]
        // chunk.len() <= HASH_BLOCK_BYTES (1 MiB) which fits in u64 on all platforms.
        {
            total += chunk.len() as u64;
        }
    }

    // 3. fsync the temp file.
    tmp.as_file().sync_all()?;

    // 4. Atomic rename onto the target.
    tmp.persist(target)
        .map_err(|e| LocalFsAdapterError::from(e.error))?;

    // 5. fsync the directory so the rename is durable.
    let dir = std::fs::File::open(parent)?;
    dir.sync_all()?;

    // 6. Stat the result for the FileSide.
    let meta = std::fs::metadata(target)?;
    let mtime_sys = meta.modified()?;
    let mtime_chrono: chrono::DateTime<chrono::Utc> = mtime_sys.into();
    let digest: [u8; 32] = *hasher.finalize().as_bytes();

    Ok(FileSide {
        kind: FileKind::File,
        size_bytes: total,
        content_hash: Some(ContentHash::from_bytes(digest)),
        mtime: Timestamp::from_datetime(mtime_chrono),
        etag: None,
        remote_item_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn write_atomic_writes_bytes_to_target() {
        let tmp = TempDir::new().expect("tmpdir");
        let target = tmp.path().join("out.bin");
        let side = write_atomic(&target, b"hello onesync").expect("write");
        let read_back = std::fs::read(&target).expect("read back");
        assert_eq!(read_back, b"hello onesync");
        assert_eq!(side.size_bytes, 13);
        assert_eq!(side.kind, FileKind::File);
        assert!(side.content_hash.is_some());
    }

    #[test]
    fn write_atomic_returns_side_with_blake3_hash() {
        let tmp = TempDir::new().expect("tmpdir");
        let target = tmp.path().join("hashed.bin");
        let bytes = b"the quick brown fox";
        let side = write_atomic(&target, bytes).expect("write");
        let expected = blake3::hash(bytes);
        assert_eq!(
            side.content_hash.expect("hash").as_bytes(),
            expected.as_bytes()
        );
    }

    #[test]
    fn write_atomic_replaces_existing_file() {
        let tmp = TempDir::new().expect("tmpdir");
        let target = tmp.path().join("replace.bin");
        std::fs::write(&target, b"v1").expect("v1");
        let _ = write_atomic(&target, b"v2-longer").expect("v2");
        assert_eq!(std::fs::read(&target).unwrap(), b"v2-longer");
    }

    #[test]
    fn write_atomic_rejects_target_without_parent() {
        // Root directory has no parent; on macOS, `/` parent is None.
        let err = write_atomic(Path::new("/"), b"x").expect_err("no parent");
        assert!(matches!(err, LocalFsAdapterError::InvalidPath { .. }));
    }

    #[test]
    fn write_atomic_handles_block_boundary_sizes() {
        let tmp = TempDir::new().expect("tmpdir");
        let target = tmp.path().join("block.bin");
        let bytes = vec![0xCC_u8; HASH_BLOCK_BYTES + 7]; // straddles boundary
        let side = write_atomic(&target, &bytes).expect("write");
        assert_eq!(side.size_bytes, bytes.len() as u64);
    }

    #[test]
    fn write_atomic_dropping_tmp_does_not_create_target() {
        // Demonstrates the "partial write" safety: if we never call persist, the
        // target file does not exist.
        let tmp = TempDir::new().expect("tmpdir");
        let parent = tmp.path();
        let target = parent.join("never_persisted.bin");
        {
            let _tmp_file = tempfile::NamedTempFile::new_in(parent).expect("tmp");
            // Dropped without persisting.
        }
        assert!(!target.exists());
    }
}
