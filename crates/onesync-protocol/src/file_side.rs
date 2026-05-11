//! Snapshot of one side's view of a file at a point in time.

use serde::{Deserialize, Serialize};

use crate::enums::FileKind;
use crate::primitives::{ContentHash, DriveItemId, ETag, Timestamp};

/// Snapshot of one side's (local or remote) view of a single file or directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSide {
    /// Whether this entry is a regular file or a directory.
    pub kind: FileKind,
    /// Size of the file content in bytes (0 for directories).
    pub size_bytes: u64,
    /// BLAKE3 content hash; `None` for directories or when not yet computed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<ContentHash>,
    /// Last-modified time reported by the owning side.
    pub mtime: Timestamp,
    /// `OneDrive` ETag/cTag; present only on the remote side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<ETag>,
    /// `OneDrive` driveItem id; present only on the remote side.
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
            self.kind == FileKind::Directory
                || self.content_hash.is_some()
                || other.kind == FileKind::Directory
                || other.content_hash.is_some(),
            "file sides ought to carry hashes for equality checks"
        );
        self.kind == other.kind
            && self.size_bytes == other.size_bytes
            && self.content_hash == other.content_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enums::FileKind;
    use crate::primitives::Timestamp;
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
