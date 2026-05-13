//! Bounded BFS directory scan.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use onesync_core::limits::SCAN_QUEUE_DEPTH_MAX;
use onesync_protocol::{enums::FileKind, primitives::Timestamp};

use crate::error::LocalFsAdapterError;

/// Metadata captured by a scan pass. Hashing happens lazily later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFileMeta {
    /// Absolute path on the host.
    pub path: PathBuf,
    /// File or directory.
    pub kind: FileKind,
    /// Size in bytes (0 for directories).
    pub size_bytes: u64,
    /// Modification time.
    pub mtime: Timestamp,
}

const SKIP_NAMES: &[&str] = &[".DS_Store", "Icon\r", ".localized"];

/// Classification returned by [`classify_skip`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Symbolic link.
    Symlink,
    /// macOS resource-fork sidecar (`._*`).
    ResourceFork,
    /// Filename in the platform deny-list (`.DS_Store`, etc.).
    DenyListed,
    /// Filename is not valid UTF-8.
    NonUtf8,
}

fn classify_skip(name: &std::ffi::OsStr, meta: &std::fs::Metadata) -> Option<SkipReason> {
    if meta.file_type().is_symlink() {
        return Some(SkipReason::Symlink);
    }
    if let Some(s) = name.to_str() {
        if s.starts_with("._") {
            return Some(SkipReason::ResourceFork);
        }
        if SKIP_NAMES.contains(&s) {
            return Some(SkipReason::DenyListed);
        }
        None
    } else {
        Some(SkipReason::NonUtf8)
    }
}

/// Walk `root` breadth-first, returning metadata for every file and directory.
///
/// # Errors
/// Returns `LocalFsAdapterError::Io` on any `read_dir`/`metadata` failure that isn't
/// "permission denied" on a single subtree (which is still surfaced — the caller decides).
/// Returns `LocalFsAdapterError::InvalidPath` if the BFS queue exceeds `SCAN_QUEUE_DEPTH_MAX`.
pub fn scan(root: &Path) -> Result<Vec<LocalFileMeta>, LocalFsAdapterError> {
    let (entries, _) = scan_with_skips(root)?;
    Ok(entries)
}

/// Variant of [`scan`] that also returns symlink-skipped paths so the scheduler can emit
/// `local.symlink.skipped` audit events.
///
/// # Errors
/// Same as [`scan`].
pub fn scan_with_skips(
    root: &Path,
) -> Result<(Vec<LocalFileMeta>, Vec<PathBuf>), LocalFsAdapterError> {
    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    queue.push_back(root.to_path_buf());

    let mut out: Vec<LocalFileMeta> = Vec::new();
    let mut symlinks: Vec<PathBuf> = Vec::new();

    while let Some(dir) = queue.pop_front() {
        let entries = std::fs::read_dir(&dir)?;
        for entry in entries {
            let entry = entry?;
            let meta = entry.metadata()?;
            let name = entry.file_name();
            if let Some(reason) = classify_skip(&name, &meta) {
                if reason == SkipReason::Symlink {
                    symlinks.push(entry.path());
                }
                continue;
            }
            let path = entry.path();
            let mtime_sys = meta.modified()?;
            let mtime_chrono: chrono::DateTime<chrono::Utc> = mtime_sys.into();
            let mtime = Timestamp::from_datetime(mtime_chrono);

            if meta.is_dir() {
                if queue.len() >= SCAN_QUEUE_DEPTH_MAX {
                    return Err(LocalFsAdapterError::InvalidPath {
                        reason: format!(
                            "scan queue overflow at {}; raise SCAN_QUEUE_DEPTH_MAX or split the pair",
                            path.display()
                        ),
                    });
                }
                queue.push_back(path.clone());
                out.push(LocalFileMeta {
                    path,
                    kind: FileKind::Directory,
                    size_bytes: 0,
                    mtime,
                });
            } else if meta.is_file() {
                out.push(LocalFileMeta {
                    path,
                    kind: FileKind::File,
                    size_bytes: meta.len(),
                    mtime,
                });
            }
            // (anything else — sockets, fifos, block devices — silently skipped)
        }
    }

    Ok((out, symlinks))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).expect("create");
        f.write_all(content).expect("write");
        p
    }

    #[test]
    fn scan_empty_root_returns_no_entries() {
        let tmp = TempDir::new().expect("tmpdir");
        let entries = scan(tmp.path()).expect("scan");
        assert!(entries.is_empty());
    }

    #[test]
    fn scan_returns_files_and_directories() {
        let tmp = TempDir::new().expect("tmpdir");
        write_file(tmp.path(), "a.txt", b"a");
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        write_file(&sub, "b.txt", b"bb");

        let mut entries = scan(tmp.path()).expect("scan");
        entries.sort_by(|a, b| a.path.cmp(&b.path));

        assert_eq!(entries.len(), 3); // a.txt, sub/, sub/b.txt
        let kinds: Vec<_> = entries.iter().map(|e| e.kind).collect();
        assert_eq!(kinds.iter().filter(|k| **k == FileKind::File).count(), 2);
        assert_eq!(
            kinds.iter().filter(|k| **k == FileKind::Directory).count(),
            1
        );
    }

    #[test]
    fn scan_recurses_three_levels_deep() {
        let tmp = TempDir::new().expect("tmpdir");
        let a = tmp.path().join("a");
        let b = a.join("b");
        let c = b.join("c");
        std::fs::create_dir_all(&c).unwrap();
        write_file(&c, "deep.txt", b"deep");

        let entries = scan(tmp.path()).expect("scan");
        let has_deep = entries
            .iter()
            .any(|e| e.path.file_name() == Some(std::ffi::OsStr::new("deep.txt")));
        assert!(has_deep);
    }

    #[test]
    fn scan_skips_ds_store() {
        let tmp = TempDir::new().expect("tmpdir");
        write_file(tmp.path(), ".DS_Store", b"mac junk");
        write_file(tmp.path(), "real.txt", b"keep");

        let entries = scan(tmp.path()).expect("scan");
        let names: Vec<_> = entries
            .iter()
            .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(!names.contains(&".DS_Store".to_string()));
        assert!(names.contains(&"real.txt".to_string()));
    }

    #[test]
    fn scan_skips_resource_fork_sidecars() {
        let tmp = TempDir::new().expect("tmpdir");
        write_file(tmp.path(), "._hidden", b"resource fork");
        write_file(tmp.path(), "visible.txt", b"keep");

        let entries = scan(tmp.path()).expect("scan");
        let names: Vec<_> = entries
            .iter()
            .map(|e| e.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(!names.iter().any(|n| n.starts_with("._")));
        assert!(names.contains(&"visible.txt".to_string()));
    }

    #[test]
    fn scan_skips_symlinks() {
        let tmp = TempDir::new().expect("tmpdir");
        let target = write_file(tmp.path(), "target.txt", b"target");
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        let entries = scan(tmp.path()).expect("scan");
        let has_link = entries.iter().any(|e| e.path == link);
        assert!(!has_link, "symlinks must be skipped");
    }

    #[test]
    #[allow(clippy::disallowed_methods)] // LINT: test-only wall-clock anchor
    fn scan_reports_mtime_within_recent_window() {
        let tmp = TempDir::new().expect("tmpdir");
        write_file(tmp.path(), "now.txt", b"x");

        let entries = scan(tmp.path()).expect("scan");
        let entry = entries.first().expect("at least one");
        let now = chrono::Utc::now();
        let delta = (now - entry.mtime.into_inner()).num_seconds().abs();
        assert!(delta < 10, "mtime should be within 10s of now");
    }
}
