//! `onesyncd --check` probes.
//!
//! Each probe returns a [`CheckResult`] capturing a name, a pass/fail/warn status, and
//! an operator-readable detail string.
//!
//! Probes are side-effect-free: the keychain probe reads a never-stored synthetic
//! account, the `FSEvents` probe watches a scratch directory, and the full-disk-access
//! probe only reads a TCC-protected path.
//!
//! The aggregate exit code is 0 when no probe `fail`s; warnings still exit 0 so the
//! command stays cheap to run in CI.

use std::path::Path;

use serde::Serialize;

/// Outcome of one probe.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CheckResult {
    /// Short identifier (`state_store`, `keychain`, …).
    pub name: &'static str,
    /// `"pass"`, `"warn"`, or `"fail"`.
    pub status: &'static str,
    /// Operator-readable explanation of the result.
    pub detail: String,
}

impl CheckResult {
    fn pass(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: "pass",
            detail: detail.into(),
        }
    }
    fn warn(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: "warn",
            detail: detail.into(),
        }
    }
    fn fail(name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            name,
            status: "fail",
            detail: detail.into(),
        }
    }
}

/// Whether the aggregated check set should exit 0 (no fails) or 1.
#[must_use]
pub fn aggregate_exit_code(results: &[CheckResult]) -> i32 {
    i32::from(results.iter().any(|r| r.status == "fail"))
}

/// State-store probe: opens the `SQLite` database, which forces migrations to run.
#[must_use]
pub fn check_state_store(state_dir: &Path) -> CheckResult {
    match crate::wiring::build_ports(state_dir) {
        Ok(_) => CheckResult::pass(
            "state_store",
            format!("opened state store under {}", state_dir.display()),
        ),
        Err(e) => CheckResult::fail("state_store", format!("build_ports failed: {e}")),
    }
}

/// Keychain reachability probe: looks up a synthetic, never-stored account id.
///
/// `errSecItemNotFound` (the expected outcome) → pass; a successful read of the synthetic
/// id → warn (collision with a real entry — pick a fresh probe id); any other error →
/// fail (the surface is unreachable).
pub async fn check_keychain() -> CheckResult {
    use onesync_core::ports::VaultError;
    use onesync_protocol::id::AccountId;

    // Fixed probe id picked so the lookup never collides with a real account.
    let Ok(probe_id) = "acct_00FZFGZFGZFGZFGZFGZFGZFGZG".parse::<AccountId>() else {
        return CheckResult::fail("keychain", "internal: probe id failed to parse".to_owned());
    };

    let vault: Box<dyn onesync_core::ports::TokenVault> =
        Box::new(onesync_keychain::KeychainTokenVault);

    match vault.load_refresh(&probe_id).await {
        Ok(_) => CheckResult::warn(
            "keychain",
            "probe id unexpectedly mapped to a stored credential — pick a fresh probe id".to_owned(),
        ),
        Err(VaultError::NotFound) => CheckResult::pass(
            "keychain",
            "keychain reachable (probe absent, as expected)".to_owned(),
        ),
        Err(e) => CheckResult::fail("keychain", format!("keychain unreachable: {e}")),
    }
}

/// `FSEvents` probe: watch a temp directory, touch a file, expect an event within 2s.
///
/// The probe creates its own scratch directory under `$TMPDIR` (or `/tmp`) with a
/// pid-derived suffix and best-effort removes it on completion. Failures during
/// cleanup do not change the probe outcome.
pub async fn check_fsevents() -> CheckResult {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let scratch = std::env::temp_dir().join(format!(
        "onesync-check-{}-{}",
        std::process::id(),
        nonce
    ));
    if let Err(e) = std::fs::create_dir_all(&scratch) {
        return CheckResult::fail("fsevents", format!("scratch dir failed: {e}"));
    }
    let result = run_fsevents_probe(&scratch).await;
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

async fn run_fsevents_probe(scratch: &std::path::Path) -> CheckResult {
    use onesync_core::ports::{LocalEventDto, LocalFs as _};
    use onesync_protocol::path::AbsPath;
    use std::time::Duration;

    let Some(root_str) = scratch.to_str() else {
        return CheckResult::warn("fsevents", "scratch path is not UTF-8; skipped".to_owned());
    };
    let Ok(root) = root_str.parse::<AbsPath>() else {
        return CheckResult::warn(
            "fsevents",
            format!("scratch path is not absolute-NFC: {root_str}"),
        );
    };

    let adapter = onesync_fs_local::LocalFsAdapter;
    let mut stream = match adapter.watch(&root).await {
        Ok(s) => s,
        Err(e) => return CheckResult::fail("fsevents", format!("watch failed: {e}")),
    };

    tokio::time::sleep(Duration::from_millis(250)).await;
    let probe_path = scratch.join("onesync-check-probe.txt");
    if let Err(e) = std::fs::write(&probe_path, b"probe") {
        return CheckResult::fail("fsevents", format!("write probe failed: {e}"));
    }

    match tokio::time::timeout(Duration::from_secs(2), stream.0.recv()).await {
        Ok(Some(LocalEventDto::Created(_) | LocalEventDto::Modified(_))) => {
            CheckResult::pass("fsevents", "`FSEvents` delivered a probe write event".to_owned())
        }
        Ok(Some(other)) => CheckResult::warn(
            "fsevents",
            format!("`FSEvents` delivered an unexpected first event: {other:?}"),
        ),
        Ok(None) => CheckResult::fail("fsevents", "`FSEvents` stream closed unexpectedly".to_owned()),
        Err(_) => CheckResult::warn(
            "fsevents",
            "no `FSEvents` notification within 2s — grant Full Disk Access or check entitlements"
                .to_owned(),
        ),
    }
}

/// Full Disk Access probe: try to read `~/Library/Mail`.
///
/// macOS gates this directory behind TCC; a `PermissionDenied` confirms FDA has not
/// been granted. Missing directory or any other error becomes a `warn`
/// (probe inconclusive).
#[must_use]
pub fn check_full_disk_access() -> CheckResult {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return CheckResult::warn("full_disk_access", "HOME unset; probe skipped".to_owned());
    };
    let mail = home.join("Library").join("Mail");
    match std::fs::read_dir(&mail) {
        Ok(_) => CheckResult::pass(
            "full_disk_access",
            format!("read {} — Full Disk Access appears granted", mail.display()),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => CheckResult::warn(
            "full_disk_access",
            "Full Disk Access denied; grant onesyncd in System Settings → \
             Privacy & Security → Full Disk Access if you intend to sync from \
             TCC-protected directories"
                .to_owned(),
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => CheckResult::warn(
            "full_disk_access",
            format!("{} not present; probe inconclusive", mail.display()),
        ),
        Err(e) => CheckResult::warn("full_disk_access", format!("probe error: {e}")),
    }
}
