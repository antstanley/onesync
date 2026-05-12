//! Conflict-detection and loser-rename policy.

use onesync_protocol::{
    enums::ConflictSide,
    path::{PathParseError, RelPath},
    primitives::Timestamp,
};

use crate::limits::{CONFLICT_MTIME_TOLERANCE_MS, CONFLICT_RENAME_RETRIES};

/// Choose which side wins and produce the loser's rename path.
///
/// The winner is the side whose `mtime` is strictly newer by more than
/// [`CONFLICT_MTIME_TOLERANCE_MS`]. When the difference is within the
/// tolerance window the remote side wins (server is authoritative).
///
/// # Arguments
///
/// * `local_mtime` — last-modified time of the local file.
/// * `remote_mtime` — last-modified time of the remote file.
/// * `relative_path` — path where both copies currently live.
/// * `host_name` — short host identifier used to disambiguate the loser copy.
/// * `attempt` — zero-indexed retry counter; appended to the loser name when > 0.
///
/// # Errors
///
/// Returns [`PathParseError`] if the constructed loser path is invalid.
pub fn pick_winner_and_loser(
    local_mtime: Timestamp,
    remote_mtime: Timestamp,
    relative_path: &RelPath,
    host_name: &str,
    attempt: u32,
) -> Result<ConflictOutcome, PathParseError> {
    let local_dt = local_mtime.into_inner();
    let remote_dt = remote_mtime.into_inner();
    let diff_ms = (local_dt - remote_dt).num_milliseconds().unsigned_abs();

    let winner = if diff_ms > CONFLICT_MTIME_TOLERANCE_MS && local_dt > remote_dt {
        ConflictSide::Local
    } else {
        ConflictSide::Remote
    };

    let loser_path = build_loser_path(relative_path, host_name, attempt)?;
    Ok(ConflictOutcome { winner, loser_path })
}

/// Outcome of the conflict-winner selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictOutcome {
    /// Side whose content is kept at the original path.
    pub winner: ConflictSide,
    /// Path where the losing copy is saved.
    pub loser_path: RelPath,
}

/// Build the loser rename path.
///
/// Pattern: `<stem> (conflict copy from <host_name>)[.<attempt>].<ext>`
///
/// # Errors
///
/// Returns [`PathParseError`] if the constructed path is invalid.
pub fn build_loser_path(
    original: &RelPath,
    host_name: &str,
    attempt: u32,
) -> Result<RelPath, PathParseError> {
    let s = original.as_str();
    let (dir, file) = s.rfind('/').map_or(("", s), |i| (&s[..i], &s[i + 1..]));
    let (stem, ext) = file
        .rfind('.')
        .map_or((file, ""), |i| (&file[..i], &file[i..]));

    let suffix = if attempt == 0 {
        String::new()
    } else {
        format!(".{attempt}")
    };
    let loser_name = format!("{stem} (conflict copy from {host_name}){suffix}{ext}");
    let loser_path = if dir.is_empty() {
        loser_name
    } else {
        format!("{dir}/{loser_name}")
    };
    loser_path.parse()
}

/// Return the maximum number of loser-rename attempts.
#[must_use]
pub const fn rename_retry_limit() -> u32 {
    CONFLICT_RENAME_RETRIES
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_datetime(Utc.timestamp_opt(secs, 0).unwrap())
    }

    fn relpath(s: &str) -> RelPath {
        s.parse().unwrap()
    }

    #[test]
    fn local_wins_when_clearly_newer() {
        // local is 5 s newer, tolerance is 1 s
        let out = pick_winner_and_loser(ts(1000), ts(995), &relpath("doc.txt"), "mac", 0).unwrap();
        assert_eq!(out.winner, ConflictSide::Local);
    }

    #[test]
    fn remote_wins_within_tolerance() {
        // 500 ms difference — within 1 s tolerance
        let local = Timestamp::from_datetime(
            Utc.timestamp_opt(1000, 0).unwrap() + chrono::Duration::milliseconds(500),
        );
        let remote = Timestamp::from_datetime(Utc.timestamp_opt(1000, 0).unwrap());
        let out = pick_winner_and_loser(local, remote, &relpath("doc.txt"), "mac", 0).unwrap();
        assert_eq!(out.winner, ConflictSide::Remote);
    }

    #[test]
    fn remote_wins_when_clearly_newer() {
        let out = pick_winner_and_loser(ts(990), ts(1000), &relpath("doc.txt"), "mac", 0).unwrap();
        assert_eq!(out.winner, ConflictSide::Remote);
    }

    #[test]
    fn loser_path_has_conflict_suffix() {
        let p = build_loser_path(&relpath("docs/notes.txt"), "macbook", 0).unwrap();
        assert_eq!(p.as_str(), "docs/notes (conflict copy from macbook).txt");
    }

    #[test]
    fn loser_path_includes_attempt_when_nonzero() {
        let p = build_loser_path(&relpath("docs/notes.txt"), "macbook", 2).unwrap();
        assert_eq!(p.as_str(), "docs/notes (conflict copy from macbook).2.txt");
    }

    #[test]
    fn loser_path_no_directory_component() {
        let p = build_loser_path(&relpath("file.md"), "host", 0).unwrap();
        assert_eq!(p.as_str(), "file (conflict copy from host).md");
    }

    #[test]
    fn rename_retry_limit_equals_const() {
        assert_eq!(rename_retry_limit(), CONFLICT_RENAME_RETRIES);
    }
}
