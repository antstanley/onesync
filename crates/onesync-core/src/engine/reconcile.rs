//! Pure reconciliation: `(synced, local, remote)` → `Decision`.

use std::collections::BTreeSet;

use onesync_protocol::{
    enums::ConflictSide, file_side::FileSide, path::RelPath, primitives::Timestamp,
};

use crate::engine::conflict::loser_rename_target;
use crate::engine::types::Decision;
use crate::limits::CONFLICT_MTIME_TOLERANCE_MS;

/// Compute the engine's decision for a single path.
///
/// `host` is used in conflict loser-rename naming.
/// `existing` is the set of relative paths already in use under the same pair —
///   passed so the conflict path collision check can avoid landing the renamed loser
///   on top of another entry.
#[must_use]
pub fn reconcile(
    relative_path: &RelPath,
    synced: Option<&FileSide>,
    local: Option<&FileSide>,
    remote: Option<&FileSide>,
    host: &str,
    detected_at: Timestamp,
    existing: &BTreeSet<RelPath>,
) -> Decision {
    let local_diff = sides_diverge(synced, local);
    let remote_diff = sides_diverge(synced, remote);

    match (local_diff, remote_diff) {
        (false, false) => Decision::Clean,
        (true, false) => match (synced, local) {
            (Some(_), None) => Decision::DeleteRemote,
            _ => Decision::UploadLocalToRemote,
        },
        (false, true) => match (synced, remote) {
            (Some(_), None) => Decision::DeleteLocal,
            _ => Decision::DownloadRemoteToLocal,
        },
        (true, true) => {
            // Both diverged from synced. If local == remote, they converged
            // independently — mark Clean.
            if sides_content_equal(local, remote) {
                return Decision::Clean;
            }
            // Otherwise, run conflict policy.
            let winner = choose_winner(local, remote);
            let Some(loser_target) =
                loser_rename_target(relative_path, detected_at, host, existing)
            else {
                // Exhausted retries — fall back to Clean and surface in audit
                // (caller logs `conflict.unresolvable`).
                return Decision::Clean;
            };
            Decision::Conflict {
                winner,
                loser_target,
            }
        }
    }
}

fn sides_diverge(a: Option<&FileSide>, b: Option<&FileSide>) -> bool {
    !sides_content_equal(a, b)
}

fn sides_content_equal(a: Option<&FileSide>, b: Option<&FileSide>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(_), None) | (None, Some(_)) => false,
        (Some(x), Some(y)) => x.identifies_same_content_as(y),
    }
}

fn choose_winner(local: Option<&FileSide>, remote: Option<&FileSide>) -> ConflictSide {
    // Mtime tie-break per spec: newer wins; within tolerance, remote wins.
    let l_mtime = local.map(|s| s.mtime.into_inner());
    let r_mtime = remote.map(|s| s.mtime.into_inner());
    match (l_mtime, r_mtime) {
        (Some(l), Some(r)) => {
            let l_ms = l.timestamp_millis();
            let r_ms = r.timestamp_millis();
            // LINT: absolute delta fits u64; signs cancel out.
            #[allow(clippy::cast_sign_loss)]
            let delta_ms = (l_ms - r_ms).unsigned_abs();
            if delta_ms <= CONFLICT_MTIME_TOLERANCE_MS {
                return ConflictSide::Remote;
            }
            if l_ms > r_ms {
                ConflictSide::Local
            } else {
                ConflictSide::Remote
            }
        }
        (Some(_), None) => ConflictSide::Local,
        // Either remote-only or neither side: remote wins (arbitrary but deterministic).
        (None, _) => ConflictSide::Remote,
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use onesync_protocol::{
        enums::FileKind,
        primitives::{ContentHash, Timestamp},
    };

    fn rel(s: &str) -> RelPath {
        s.parse().expect("rel")
    }

    fn side(size: u64, hash_seed: u8, mtime_secs: i64) -> FileSide {
        let bytes = [hash_seed; 32];
        FileSide {
            kind: FileKind::File,
            size_bytes: size,
            content_hash: Some(ContentHash::from_bytes(bytes)),
            mtime: Timestamp::from_datetime(Utc.timestamp_opt(mtime_secs, 0).unwrap()),
            etag: None,
            remote_item_id: None,
        }
    }

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_datetime(Utc.timestamp_opt(secs, 0).unwrap())
    }

    #[test]
    fn all_equal_yields_clean() {
        let s = side(10, 1, 0);
        let d = reconcile(
            &rel("f"),
            Some(&s),
            Some(&s),
            Some(&s),
            "h",
            ts(0),
            &BTreeSet::new(),
        );
        assert_eq!(d, Decision::Clean);
    }

    #[test]
    fn local_differs_yields_upload() {
        let synced = side(10, 1, 0);
        let local = side(10, 2, 100);
        let d = reconcile(
            &rel("f"),
            Some(&synced),
            Some(&local),
            Some(&synced),
            "h",
            ts(0),
            &BTreeSet::new(),
        );
        assert_eq!(d, Decision::UploadLocalToRemote);
    }

    #[test]
    fn remote_differs_yields_download() {
        let synced = side(10, 1, 0);
        let remote = side(10, 3, 100);
        let d = reconcile(
            &rel("f"),
            Some(&synced),
            Some(&synced),
            Some(&remote),
            "h",
            ts(0),
            &BTreeSet::new(),
        );
        assert_eq!(d, Decision::DownloadRemoteToLocal);
    }

    #[test]
    fn both_differ_distinct_yields_conflict() {
        let synced = side(10, 1, 0);
        let local = side(10, 2, 100);
        let remote = side(10, 3, 200);
        let d = reconcile(
            &rel("a.md"),
            Some(&synced),
            Some(&local),
            Some(&remote),
            "h",
            ts(0),
            &BTreeSet::new(),
        );
        match d {
            Decision::Conflict { winner, .. } => assert_eq!(winner, ConflictSide::Remote),
            _ => panic!("expected Conflict"),
        }
    }

    #[test]
    fn both_differ_but_converge_to_same_content_yields_clean() {
        let synced = side(10, 1, 0);
        let convergent = side(10, 2, 100);
        let d = reconcile(
            &rel("f"),
            Some(&synced),
            Some(&convergent),
            Some(&convergent),
            "h",
            ts(0),
            &BTreeSet::new(),
        );
        assert_eq!(d, Decision::Clean);
    }

    #[test]
    fn local_removed_yields_delete_remote() {
        let synced = side(10, 1, 0);
        let d = reconcile(
            &rel("f"),
            Some(&synced),
            None,
            Some(&synced),
            "h",
            ts(0),
            &BTreeSet::new(),
        );
        assert_eq!(d, Decision::DeleteRemote);
    }

    #[test]
    fn remote_removed_yields_delete_local() {
        let synced = side(10, 1, 0);
        let d = reconcile(
            &rel("f"),
            Some(&synced),
            Some(&synced),
            None,
            "h",
            ts(0),
            &BTreeSet::new(),
        );
        assert_eq!(d, Decision::DeleteLocal);
    }

    #[test]
    fn newer_local_wins_when_diverged_beyond_tolerance() {
        let synced = side(10, 1, 0);
        let local = side(10, 2, 1_000_000); // way newer
        let remote = side(10, 3, 100);
        let d = reconcile(
            &rel("a.md"),
            Some(&synced),
            Some(&local),
            Some(&remote),
            "h",
            ts(0),
            &BTreeSet::new(),
        );
        match d {
            Decision::Conflict { winner, .. } => assert_eq!(winner, ConflictSide::Local),
            _ => panic!("expected Conflict"),
        }
    }

    #[test]
    fn new_file_on_both_sides_diverged_yields_conflict() {
        // synced = None means first-seen; both sides have different content
        let local = side(10, 2, 100);
        let remote = side(10, 3, 200);
        let d = reconcile(
            &rel("new.txt"),
            None,
            Some(&local),
            Some(&remote),
            "h",
            ts(0),
            &BTreeSet::new(),
        );
        assert!(matches!(d, Decision::Conflict { .. }));
    }
}
