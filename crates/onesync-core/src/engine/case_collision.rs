//! Case-insensitive name-collision detection and rename helpers.
//!
//! Per the [01-domain-model decision](../../../../docs/spec/01-domain-model.md), on case-
//! sensitive APFS volumes the local side can legitimately hold `Report.pdf` and `report.pdf`
//! side by side while `OneDrive` treats them as the same file. When the engine encounters
//! this it picks `OneDrive`'s stored-name spelling as canonical and renames the local-side
//! challenger with a
//! `(case-collision-<short-hash>)` suffix so it flows through the normal sync pipeline as a
//! new file. The renamed entry also gets a `Conflict` row recorded for review.
//!
//! This module exposes the rename-target derivation as a pure function. The scheduler in
//! `crates/onesync-daemon/src/scheduler.rs` calls it during reconcile when a local scan
//! observes two entries whose NFC-normalised lowercase forms match.

use blake3::Hasher;

use onesync_protocol::path::RelPath;

/// Length (hex chars) of the BLAKE3-derived collision suffix. Seven hex chars give 2^28 ≈
/// 268M values; collisions within a single folder are astronomically unlikely.
const HASH_HEX_LEN: usize = 7;

/// Derive the rename target for a local-side path that case-collides with its remote
/// counterpart.
///
/// The result inserts ` (case-collision-<short>)` immediately before the final extension.
/// `<short>` is the first [`HASH_HEX_LEN`] hex chars of `BLAKE3(original_path_bytes)` — pure
/// so repeated reconcile passes settle on the same name.
///
/// Examples (with a representative `<short>`):
/// - `Documents/Report.pdf` → `Documents/Report (case-collision-1a2b3c4).pdf`
/// - `Notes/file` (no extension) → `Notes/file (case-collision-1a2b3c4)`
/// - `archive.tar.gz` → `archive.tar (case-collision-1a2b3c4).gz`
#[must_use]
pub fn case_collision_rename_target(original: &RelPath) -> String {
    let original_str = original.as_str();
    let short = short_hash(original_str.as_bytes());
    let (parent, basename) = original_str.rfind('/').map_or(("", original_str), |idx| {
        (&original_str[..=idx], &original_str[idx + 1..])
    });
    let (stem, ext) = match basename.rfind('.') {
        // Treat dotfiles (`.bashrc`) as all-stem, no-ext.
        Some(idx) if idx > 0 => (&basename[..idx], &basename[idx..]),
        _ => (basename, ""),
    };
    format!("{parent}{stem} (case-collision-{short}){ext}")
}

fn short_hash(bytes: &[u8]) -> String {
    let mut h = Hasher::new();
    h.update(bytes);
    let digest = h.finalize();
    let hex = digest.to_hex();
    hex.as_str()[..HASH_HEX_LEN].to_owned()
}

/// Returns `true` if two relative paths case-fold to the same string.
///
/// The comparison is ASCII-lowercase only — extending to full Unicode case folding is
/// possible later if real-world filenames need it.
#[must_use]
pub fn case_folds_equal(a: &RelPath, b: &RelPath) -> bool {
    a.as_str().eq_ignore_ascii_case(b.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> RelPath {
        s.parse().expect("rel path")
    }

    #[test]
    fn rename_target_preserves_extension() {
        let target = case_collision_rename_target(&rp("Documents/Report.pdf"));
        assert!(target.starts_with("Documents/Report (case-collision-"));
        assert!(target.ends_with(").pdf"));
    }

    #[test]
    fn rename_target_handles_no_extension() {
        let target = case_collision_rename_target(&rp("Notes/file"));
        assert!(target.starts_with("Notes/file (case-collision-"));
        assert!(!target.contains('.'));
    }

    #[test]
    fn rename_target_handles_double_extension() {
        // Only the trailing extension is preserved; the convention matches GNU mv.
        let target = case_collision_rename_target(&rp("archive.tar.gz"));
        assert!(target.starts_with("archive.tar (case-collision-"));
        assert!(target.ends_with(").gz"));
    }

    #[test]
    fn rename_target_is_deterministic() {
        let a = case_collision_rename_target(&rp("Documents/Report.pdf"));
        let b = case_collision_rename_target(&rp("Documents/Report.pdf"));
        assert_eq!(a, b);
    }

    #[test]
    fn rename_target_diverges_for_different_inputs() {
        let a = case_collision_rename_target(&rp("Documents/Report.pdf"));
        let b = case_collision_rename_target(&rp("Documents/report.pdf"));
        assert_ne!(a, b);
    }

    #[test]
    fn case_folds_equal_matches_ascii_case() {
        assert!(case_folds_equal(&rp("Report.PDF"), &rp("report.pdf")));
        assert!(!case_folds_equal(
            &rp("Report.pdf"),
            &rp("Report-other.pdf")
        ));
    }

    #[test]
    fn rename_target_is_valid_rel_path() {
        // The output must parse back as a RelPath (no embedded NUL, no '..', no leading '/').
        let target = case_collision_rename_target(&rp("Documents/Report.pdf"));
        let _parsed: RelPath = target
            .parse()
            .expect("rename target must be a valid RelPath");
    }
}
