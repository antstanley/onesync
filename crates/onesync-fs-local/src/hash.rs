//! BLAKE3 content-hash helper.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::SystemTime;

use onesync_core::limits::HASH_BLOCK_BYTES;
use onesync_protocol::primitives::ContentHash;

use crate::error::LocalFsAdapterError;

/// Compute the BLAKE3 digest of the file at `path`, streaming in `HASH_BLOCK_BYTES`
/// chunks.
///
/// Returns `LocalFsAdapterError::Raced` if the file's `mtime` changed between the
/// initial metadata read and the final metadata read — that's how the engine detects
/// a concurrent write underneath the hasher.
///
/// # Errors
/// Standard I/O errors are mapped to `LocalFsAdapterError::Io`. A mid-hash mtime
/// change is mapped to `LocalFsAdapterError::Raced`.
pub fn hash(path: &Path) -> Result<ContentHash, LocalFsAdapterError> {
    let mtime_before = mtime(path)?;

    let mut file = File::open(path)?;
    let mut buf = vec![0u8; HASH_BLOCK_BYTES];
    let mut hasher = blake3::Hasher::new();
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    let mtime_after = mtime(path)?;
    if mtime_before != mtime_after {
        return Err(LocalFsAdapterError::Raced);
    }

    let digest: [u8; 32] = *hasher.finalize().as_bytes();
    Ok(ContentHash::from_bytes(digest))
}

fn mtime(path: &Path) -> Result<Option<SystemTime>, LocalFsAdapterError> {
    Ok(std::fs::metadata(path)?.modified().ok())
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
        f.sync_all().expect("sync");
        p
    }

    #[test]
    fn hash_of_empty_file_matches_blake3_of_empty() {
        let tmp = TempDir::new().expect("tmpdir");
        let p = write_file(tmp.path(), "empty.bin", &[]);
        let h = hash(&p).expect("hash");
        // BLAKE3 of empty input is a fixed value.
        assert_eq!(
            h.to_string(),
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn hash_matches_blake3_reference_for_known_input() {
        let tmp = TempDir::new().expect("tmpdir");
        let content = b"onesync test fixture";
        let p = write_file(tmp.path(), "fixture.bin", content);
        let h = hash(&p).expect("hash");
        let reference = blake3::hash(content);
        let expected: [u8; 32] = *reference.as_bytes();
        assert_eq!(h.as_bytes(), &expected);
    }

    #[test]
    fn hash_of_block_boundary_file_is_correct() {
        // File exactly HASH_BLOCK_BYTES long exercises the loop termination on n==0.
        let tmp = TempDir::new().expect("tmpdir");
        let content = vec![0xAA_u8; HASH_BLOCK_BYTES];
        let p = write_file(tmp.path(), "block.bin", &content);
        let h = hash(&p).expect("hash");
        let reference = blake3::hash(&content);
        assert_eq!(h.as_bytes(), reference.as_bytes());
    }

    #[test]
    fn hash_returns_raced_on_mtime_change() {
        let tmp = TempDir::new().expect("tmpdir");
        let p = write_file(tmp.path(), "racy.bin", b"v1");

        // Verify mtime changes across rewrites (required for the test to be meaningful).
        let pre = std::fs::metadata(&p).unwrap().modified().ok();
        // Sleep at least 1 second so file system mtime resolution registers the change.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write_file(tmp.path(), "racy.bin", b"v2");
        let post = std::fs::metadata(&p).unwrap().modified().ok();
        assert_ne!(
            pre, post,
            "mtime must change across rewrites for this test to be meaningful"
        );

        // After the file has settled, hash returns Ok with the new content's hash.
        let h = hash(&p).expect("hash after settle");
        let expected = blake3::hash(b"v2");
        assert_eq!(h.as_bytes(), expected.as_bytes());
    }
}
