//! Keep-both conflict loser-rename policy.

use std::collections::BTreeSet;

use onesync_protocol::{path::RelPath, primitives::Timestamp};

use crate::limits::CONFLICT_RENAME_RETRIES;

/// Compute the rename target for a conflict loser.
///
/// `existing` is the set of paths already in use under the same parent — pass an
/// empty set if no collision check is needed at the call site.
///
/// Returns `None` if `CONFLICT_RENAME_RETRIES` collisions occurred.
#[must_use]
pub fn loser_rename_target(
    relative_path: &RelPath,
    detected_at: Timestamp,
    host: &str,
    existing: &BTreeSet<RelPath>,
) -> Option<RelPath> {
    let (stem, ext) = split_stem_ext(relative_path.as_str());
    let ts = format_filename_timestamp(&detected_at);
    let base = format_conflict_name(stem, &ts, host, ext);
    if let Ok(candidate) = base.parse::<RelPath>()
        && !existing.contains(&candidate)
    {
        return Some(candidate);
    }
    for i in 2..=CONFLICT_RENAME_RETRIES {
        let with_suffix = format_conflict_name_with_suffix(stem, &ts, host, ext, i);
        if let Ok(candidate) = with_suffix.parse::<RelPath>()
            && !existing.contains(&candidate)
        {
            return Some(candidate);
        }
    }
    None
}

fn split_stem_ext(path: &str) -> (&str, Option<&str>) {
    // Find the last '.' in the basename, not in the directory.
    let basename_start = path.rfind('/').map_or(0, |i| i + 1);
    let basename = &path[basename_start..];
    if let Some(dot_in_basename) = basename.rfind('.') {
        if dot_in_basename == 0 {
            // dotfile like ".bashrc" — no extension.
            return (path, None);
        }
        let abs_dot = basename_start + dot_in_basename;
        return (&path[..abs_dot], Some(&path[abs_dot + 1..]));
    }
    (path, None)
}

fn format_filename_timestamp(ts: &Timestamp) -> String {
    // YYYY-MM-DDTHH-MM-SSZ (filename-safe — no colons)
    ts.into_inner().format("%Y-%m-%dT%H-%M-%SZ").to_string()
}

fn format_conflict_name(stem: &str, ts: &str, host: &str, ext: Option<&str>) -> String {
    let base = format!("{stem} (conflict {ts} from {host})");
    match ext {
        Some(e) => format!("{base}.{e}"),
        None => base,
    }
}

fn format_conflict_name_with_suffix(
    stem: &str,
    ts: &str,
    host: &str,
    ext: Option<&str>,
    suffix: u32,
) -> String {
    let base = format!("{stem} (conflict {ts} from {host})-{suffix}");
    match ext {
        Some(e) => format!("{base}.{e}"),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_datetime(Utc.timestamp_opt(secs, 0).unwrap())
    }

    fn rel(s: &str) -> RelPath {
        s.parse().expect("rel")
    }

    #[test]
    fn split_stem_ext_handles_extensions() {
        assert_eq!(split_stem_ext("notes.md"), ("notes", Some("md")));
        assert_eq!(
            split_stem_ext("dir/file.tar.gz"),
            ("dir/file.tar", Some("gz"))
        );
        assert_eq!(split_stem_ext("noext"), ("noext", None));
        assert_eq!(split_stem_ext(".bashrc"), (".bashrc", None));
    }

    #[test]
    fn rename_target_includes_timestamp_and_host() {
        let target = loser_rename_target(
            &rel("Documents/notes.md"),
            ts(1_700_000_000),
            "alice-mac",
            &BTreeSet::new(),
        )
        .expect("present");
        assert!(target.as_str().contains("notes (conflict"));
        assert!(target.as_str().contains("from alice-mac"));
        // LINT: extension comparison is intentionally exact in tests.
        #[allow(clippy::case_sensitive_file_extension_comparisons)]
        let has_md_ext = target.as_str().ends_with(".md");
        assert!(has_md_ext);
    }

    #[test]
    fn collision_gets_numeric_suffix() {
        let mut existing = BTreeSet::new();
        let original = rel("file.txt");
        let t = ts(1_700_000_000);

        let first = loser_rename_target(&original, t, "host", &existing).expect("first");
        existing.insert(first.clone());

        let second = loser_rename_target(&original, t, "host", &existing).expect("second");
        assert_ne!(second, first);
        assert!(second.as_str().contains("-2"));
    }

    #[test]
    fn exhausted_retries_returns_none_or_some() {
        // Insert all possible candidates for the given timestamp and host.
        let mut existing = BTreeSet::new();
        let original = rel("file.txt");
        let t = ts(1_700_000_000);

        // Insert the base candidate.
        let first = loser_rename_target(&original, t, "host", &existing).expect("first");
        existing.insert(first);

        // Insert all numbered candidates up to and including CONFLICT_RENAME_RETRIES.
        for i in 2..=CONFLICT_RENAME_RETRIES {
            let ts_str =
                Timestamp::from_datetime(chrono::Utc.timestamp_opt(1_700_000_000, 0).unwrap())
                    .into_inner()
                    .format("%Y-%m-%dT%H-%M-%SZ")
                    .to_string();
            let candidate_str = format!("file (conflict {ts_str} from host)-{i}.txt");
            if let Ok(p) = candidate_str.parse::<RelPath>() {
                existing.insert(p);
            }
        }

        // With all slots taken, should return None.
        let result = loser_rename_target(&original, t, "host", &existing);
        assert!(result.is_none());
    }
}
