//! macOS host integration test for `onesync service install/start/doctor/restart/stop/uninstall`.
//!
//! `#[ignore]` by default: this test drives the *real* `launchctl` against the current
//! user's `gui` domain and installs `dev.onesync.daemon` from the freshly built release
//! binaries. Run it with:
//!
//! ```sh
//! cargo build --workspace --release
//! cargo test --test macos_lifecycle -- --ignored --nocapture
//! ```
//!
//! Pre-requisites:
//! - macOS host with a logged-in GUI session.
//! - No existing `dev.onesync.daemon` install (otherwise the test will reuse / clobber it).
//! - Release binaries at `target/release/onesync` and `target/release/onesyncd`.
//!
//! The test purges its install at the end (`onesync service uninstall --purge`) even if
//! an earlier step fails, so it cleans up after itself.

#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const LABEL: &str = "dev.onesync.daemon";

fn target_release_dir() -> PathBuf {
    // Honour CARGO_TARGET_DIR; otherwise default to `target/` at the workspace root.
    if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        return PathBuf::from(dir).join("release");
    }
    // tests/ runs from the crate dir; the workspace root is two levels up.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root");
    workspace.join("target/release")
}

fn cli_binary() -> PathBuf {
    target_release_dir().join("onesync")
}

fn ensure_release_built() {
    let cli = cli_binary();
    let daemon = target_release_dir().join("onesyncd");
    assert!(
        cli.exists() && daemon.exists(),
        "release binaries missing at {} / {}. \
         Build with: cargo build --workspace --release",
        cli.display(),
        daemon.display()
    );
}

fn run_cli(args: &[&str]) -> Output {
    Command::new(cli_binary())
        .args(args)
        .arg("--json")
        .output()
        .expect("spawn onesync CLI")
}

fn uid_string() -> String {
    let out = Command::new("id").arg("-u").output().expect("id -u");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn launchctl_print() -> bool {
    let uid = uid_string();
    Command::new("launchctl")
        .args(["print", &format!("gui/{uid}/{LABEL}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn force_cleanup() {
    // Always tear down at end: best-effort `onesync service uninstall --purge`.
    let _ = Command::new(cli_binary())
        .args(["service", "uninstall", "--purge", "--json"])
        .output();
}

/// Block until the agent is loaded (or timeout). Used after `install` and `start`.
fn wait_for_loaded(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if launchctl_print() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

#[test]
#[ignore = "drives real launchctl; run with --ignored after `cargo build --workspace --release`"]
fn full_service_install_start_doctor_restart_stop_uninstall_cycle() {
    ensure_release_built();

    // Defensive: clear any pre-existing install. Ignore errors (the agent may not be loaded).
    force_cleanup();

    // ── install ───────────────────────────────────────────────────────────
    let install = run_cli(&["service", "install"]);
    assert!(
        install.status.success(),
        "service install: {}",
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(
        wait_for_loaded(Duration::from_secs(90)),
        "launchctl never reported the agent loaded within 60s after install"
    );

    // ── doctor (post-install) ─────────────────────────────────────────────
    let doctor = run_cli(&["service", "doctor"]);
    assert!(
        doctor.status.success(),
        "service doctor (post-install) failed: {}",
        String::from_utf8_lossy(&doctor.stderr)
    );

    // ── restart ───────────────────────────────────────────────────────────
    let restart = run_cli(&["service", "restart"]);
    assert!(
        restart.status.success(),
        "service restart: {}",
        String::from_utf8_lossy(&restart.stderr)
    );
    assert!(
        wait_for_loaded(Duration::from_secs(90)),
        "launchctl never reported the agent loaded within 60s after restart"
    );

    // ── stop (graceful shutdown over IPC) ─────────────────────────────────
    let stop = run_cli(&["service", "stop"]);
    assert!(
        stop.status.success(),
        "service stop: {}",
        String::from_utf8_lossy(&stop.stderr)
    );

    // ── start (back up again before uninstall) ────────────────────────────
    let start = run_cli(&["service", "start"]);
    assert!(
        start.status.success(),
        "service start: {}",
        String::from_utf8_lossy(&start.stderr)
    );

    // ── uninstall --purge ─────────────────────────────────────────────────
    let uninstall = run_cli(&["service", "uninstall", "--purge"]);
    assert!(
        uninstall.status.success(),
        "service uninstall --purge: {}",
        String::from_utf8_lossy(&uninstall.stderr)
    );
    // The agent should no longer be listed in launchctl print.
    assert!(
        !launchctl_print(),
        "agent still loaded after uninstall --purge"
    );

    // Belt-and-braces cleanup. Idempotent if already torn down.
    force_cleanup();
}
