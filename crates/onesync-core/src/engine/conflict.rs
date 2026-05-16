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
/// * `detected_at` — UTC timestamp the conflict was detected at (used in
///   the loser filename per spec).
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
    detected_at: Timestamp,
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

    let loser_path = build_loser_path(relative_path, host_name, detected_at, attempt)?;
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
/// Pattern: `<stem> (conflict <YYYY-MM-DDTHH-MM-SSZ> from <host_name>)[.<attempt>].<ext>`
/// per spec `docs/spec/03-sync-engine.md` line 178. The timestamp uses dashes
/// in the time portion so the resulting path is filesystem-safe on macOS and
/// on `OneDrive` (neither accepts `:` in file names).
///
/// # Errors
///
/// Returns [`PathParseError`] if the constructed path is invalid.
pub fn build_loser_path(
    original: &RelPath,
    host_name: &str,
    detected_at: Timestamp,
    attempt: u32,
) -> Result<RelPath, PathParseError> {
    let s = original.as_str();
    let (dir, file) = s.rfind('/').map_or(("", s), |i| (&s[..i], &s[i + 1..]));
    let (stem, ext) = file
        .rfind('.')
        .map_or((file, ""), |i| (&file[..i], &file[i..]));

    let timestamp = detected_at
        .into_inner()
        .format("%Y-%m-%dT%H-%M-%SZ")
        .to_string();
    let suffix = if attempt == 0 {
        String::new()
    } else {
        format!(".{attempt}")
    };
    let loser_name = format!("{stem} (conflict {timestamp} from {host_name}){suffix}{ext}");
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

    /// Canonical detection timestamp used in tests:
    /// `2026-05-15T10:30:45Z` → `2026-05-15T10-30-45Z` in filenames.
    fn detected_at() -> Timestamp {
        Timestamp::from_datetime(Utc.with_ymd_and_hms(2026, 5, 15, 10, 30, 45).unwrap())
    }

    #[test]
    fn local_wins_when_clearly_newer() {
        // local is 5 s newer, tolerance is 1 s
        let out = pick_winner_and_loser(
            ts(1000),
            ts(995),
            &relpath("doc.txt"),
            "mac",
            detected_at(),
            0,
        )
        .unwrap();
        assert_eq!(out.winner, ConflictSide::Local);
    }

    #[test]
    fn remote_wins_within_tolerance() {
        // 500 ms difference — within 1 s tolerance
        let local = Timestamp::from_datetime(
            Utc.timestamp_opt(1000, 0).unwrap() + chrono::Duration::milliseconds(500),
        );
        let remote = Timestamp::from_datetime(Utc.timestamp_opt(1000, 0).unwrap());
        let out =
            pick_winner_and_loser(local, remote, &relpath("doc.txt"), "mac", detected_at(), 0)
                .unwrap();
        assert_eq!(out.winner, ConflictSide::Remote);
    }

    #[test]
    fn remote_wins_when_clearly_newer() {
        let out = pick_winner_and_loser(
            ts(990),
            ts(1000),
            &relpath("doc.txt"),
            "mac",
            detected_at(),
            0,
        )
        .unwrap();
        assert_eq!(out.winner, ConflictSide::Remote);
    }

    /// RP1-F13: loser filename embeds the detection timestamp per spec
    /// `03-sync-engine.md` line 178 (`<stem> (conflict <YYYY-MM-DDTHH-MM-SSZ>
    /// from <host>).<ext>`).
    #[test]
    fn loser_path_includes_detection_timestamp() {
        let p = build_loser_path(&relpath("docs/notes.txt"), "macbook", detected_at(), 0).unwrap();
        assert_eq!(
            p.as_str(),
            "docs/notes (conflict 2026-05-15T10-30-45Z from macbook).txt"
        );
    }

    #[test]
    fn loser_path_includes_attempt_when_nonzero() {
        let p = build_loser_path(&relpath("docs/notes.txt"), "macbook", detected_at(), 2).unwrap();
        assert_eq!(
            p.as_str(),
            "docs/notes (conflict 2026-05-15T10-30-45Z from macbook).2.txt"
        );
    }

    #[test]
    fn loser_path_no_directory_component() {
        let p = build_loser_path(&relpath("file.md"), "host", detected_at(), 0).unwrap();
        assert_eq!(
            p.as_str(),
            "file (conflict 2026-05-15T10-30-45Z from host).md"
        );
    }

    #[test]
    fn rename_retry_limit_equals_const() {
        assert_eq!(rename_retry_limit(), CONFLICT_RENAME_RETRIES);
    }
}
