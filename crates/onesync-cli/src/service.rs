//! `onesync service ...` subcommands: install, uninstall, start, stop, restart,
//! doctor. Real macOS lifecycle wiring via `launchctl` shell-outs.
//!
//! See `docs/spec/08-installation-and-lifecycle.md`.

#![allow(
    clippy::collapsible_if,
    clippy::vec_init_then_push,
    clippy::disallowed_methods,
    clippy::doc_markdown
)]
// LINT: this module is the CLI-side install-time orchestration. It legitimately
//       reads env vars (HOME, PATH, USER, TMPDIR), shells out to launchctl, and
//       collects a small push-based checklist; the pedantic clippy nits below
//       don't carry their weight here.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::cli::ServiceCmd;
use crate::error::CliError;
use crate::output::{OutputCfg, emit_value};
use crate::rpc::{RpcClient, default_socket_path};

const LABEL: &str = "dev.onesync.daemon";
const PING_TIMEOUT_S: u64 = 60;

/// Dispatch a `service` subcommand.
#[allow(clippy::disallowed_methods)]
// LINT: install-time env reads (HOME, USER) precede InstanceConfig; this is the
//       startup boundary where reading env is legitimate.
pub async fn run(cfg: OutputCfg, cmd: ServiceCmd) -> Result<(), CliError> {
    match cmd {
        ServiceCmd::Install => install(cfg).await,
        ServiceCmd::Uninstall { purge } => uninstall(cfg, purge).await,
        ServiceCmd::Start => start(cfg).await,
        ServiceCmd::Stop => stop(cfg).await,
        ServiceCmd::Restart => restart(cfg).await,
        ServiceCmd::Doctor => doctor(cfg).await,
    }
}

fn home_dir() -> Result<PathBuf, CliError> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| CliError::Generic("HOME not set".into()))
}

fn uid_string() -> Result<String, CliError> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| CliError::Generic(format!("id -u: {e}")))?;
    if !out.status.success() {
        return Err(CliError::Generic("id -u failed".into()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn current_user_is_root() -> bool {
    matches!(uid_string().as_deref(), Ok("0"))
}

struct Paths {
    state_dir: PathBuf,
    log_dir: PathBuf,
    plist: PathBuf,
    daemon_binary: PathBuf,
}

impl Paths {
    fn resolve() -> Result<Self, CliError> {
        let home = home_dir()?;
        let state_dir = home.join("Library/Application Support/onesync");
        let log_dir = home.join("Library/Logs/onesync");
        let plist = home.join(format!("Library/LaunchAgents/{LABEL}.plist"));
        let daemon_binary = state_dir.join("bin/onesyncd");
        Ok(Self {
            state_dir,
            log_dir,
            plist,
            daemon_binary,
        })
    }
}

/// Render the LaunchAgent plist with the user's actual home substituted.
fn render_plist(paths: &Paths) -> String {
    let daemon = paths.daemon_binary.to_string_lossy();
    let out_log = paths
        .log_dir
        .join("onesyncd.out.log")
        .to_string_lossy()
        .into_owned();
    let err_log = paths
        .log_dir
        .join("onesyncd.err.log")
        .to_string_lossy()
        .into_owned();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
                       "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{daemon}</string>
        <string>--launchd</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key><false/>
        <key>Crashed</key><true/>
    </dict>
    <key>ThrottleInterval</key><integer>10</integer>
    <key>ProcessType</key><string>Background</string>
    <key>LowPriorityIO</key><true/>
    <key>Nice</key><integer>5</integer>
    <key>StandardOutPath</key>
    <string>{out_log}</string>
    <key>StandardErrorPath</key>
    <string>{err_log}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key><string>onesync=info</string>
    </dict>
    <key>SoftResourceLimits</key>
    <dict>
        <key>NumberOfFiles</key><integer>4096</integer>
    </dict>
</dict>
</plist>
"#
    )
}

async fn install(cfg: OutputCfg) -> Result<(), CliError> {
    if current_user_is_root() {
        return Err(CliError::Generic(
            "refusing to install as root; run as your user".into(),
        ));
    }
    let paths = Paths::resolve()?;

    // Create state-dir/bin, log-dir, LaunchAgents dir (in case it's missing).
    std::fs::create_dir_all(paths.state_dir.join("bin"))
        .and_then(|()| std::fs::create_dir_all(&paths.log_dir))
        .and_then(|()| paths.plist.parent().map_or(Ok(()), std::fs::create_dir_all))
        .map_err(|e| CliError::Generic(format!("mkdir: {e}")))?;

    // Locate the bundled onesyncd binary: alongside the running CLI, or
    // ../onesyncd, or in $PATH. Copy it to state-dir/bin/onesyncd.
    let source = locate_daemon_binary()?;
    std::fs::copy(&source, &paths.daemon_binary)
        .map_err(|e| CliError::Generic(format!("copy daemon binary: {e}")))?;
    // Make executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perm = std::fs::metadata(&paths.daemon_binary)
            .map_err(|e| CliError::Generic(format!("stat daemon: {e}")))?
            .permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&paths.daemon_binary, perm)
            .map_err(|e| CliError::Generic(format!("chmod daemon: {e}")))?;
    }

    // Write the plist.
    let plist_body = render_plist(&paths);
    std::fs::write(&paths.plist, plist_body)
        .map_err(|e| CliError::Generic(format!("write plist: {e}")))?;

    // Bootstrap + kickstart.
    let uid = uid_string()?;
    run_launchctl(&[
        "bootstrap".into(),
        format!("gui/{uid}"),
        paths.plist.to_string_lossy().into_owned(),
    ])?;
    run_launchctl(&[
        "kickstart".into(),
        "-k".into(),
        format!("gui/{uid}/{LABEL}"),
    ])?;

    // Poll health.ping for up to PING_TIMEOUT_S.
    wait_for_daemon().await?;

    emit_value(
        cfg,
        &serde_json::json!({
            "installed": true,
            "plist": paths.plist.to_string_lossy(),
            "daemon": paths.daemon_binary.to_string_lossy(),
            "socket": default_socket_path().to_string_lossy(),
        }),
    )
}

async fn uninstall(cfg: OutputCfg, purge: bool) -> Result<(), CliError> {
    let paths = Paths::resolve()?;

    // Best-effort graceful shutdown via the daemon's IPC.
    let socket = default_socket_path();
    if socket.exists() {
        if let Ok(mut client) = RpcClient::connect(&socket).await {
            let _ = client
                .call::<Value>("service.shutdown", serde_json::json!({ "drain": true }))
                .await;
        }
    }

    // bootout the agent (ignore errors — agent might not be loaded).
    let uid = uid_string()?;
    let _ = run_launchctl(&[
        "bootout".into(),
        format!("gui/{uid}"),
        paths.plist.to_string_lossy().into_owned(),
    ]);

    // Remove the plist.
    let _ = std::fs::remove_file(&paths.plist);

    let mut removed = Vec::new();
    if purge {
        let _ = std::fs::remove_dir_all(&paths.state_dir);
        let _ = std::fs::remove_dir_all(&paths.log_dir);
        removed.push("state");
        removed.push("logs");
        removed.push("daemon binary");
    }

    emit_value(
        cfg,
        &serde_json::json!({
            "uninstalled": true,
            "purged": purge,
            "removed": removed,
        }),
    )
}

async fn start(cfg: OutputCfg) -> Result<(), CliError> {
    let uid = uid_string()?;
    run_launchctl(&["kickstart".into(), format!("gui/{uid}/{LABEL}")])?;
    wait_for_daemon().await?;
    emit_value(cfg, &serde_json::json!({ "started": true }))
}

async fn stop(cfg: OutputCfg) -> Result<(), CliError> {
    let socket = default_socket_path();
    if let Ok(mut client) = RpcClient::connect(&socket).await {
        let _ = client
            .call::<Value>("service.shutdown", serde_json::json!({ "drain": true }))
            .await;
    }
    // Wait briefly for the daemon to exit.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if !socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    emit_value(cfg, &serde_json::json!({ "stopped": true }))
}

async fn restart(cfg: OutputCfg) -> Result<(), CliError> {
    stop(cfg).await?;
    start(cfg).await
}

async fn doctor(cfg: OutputCfg) -> Result<(), CliError> {
    let paths = Paths::resolve()?;
    let mut checks: Vec<(&'static str, bool, String)> = Vec::new();

    checks.push((
        "plist file exists",
        paths.plist.exists(),
        paths.plist.to_string_lossy().into_owned(),
    ));
    checks.push((
        "daemon binary exists",
        paths.daemon_binary.exists(),
        paths.daemon_binary.to_string_lossy().into_owned(),
    ));
    checks.push((
        "state directory exists",
        paths.state_dir.exists(),
        paths.state_dir.to_string_lossy().into_owned(),
    ));
    checks.push((
        "log directory exists",
        paths.log_dir.exists(),
        paths.log_dir.to_string_lossy().into_owned(),
    ));

    let socket = default_socket_path();
    let socket_alive = RpcClient::connect(&socket).await.is_ok();
    checks.push((
        "daemon responds on socket",
        socket_alive,
        socket.to_string_lossy().into_owned(),
    ));

    let all_ok = checks.iter().all(|c| c.1);
    let report = serde_json::json!({
        "ok": all_ok,
        "checks": checks
            .iter()
            .map(|(name, pass, detail)| serde_json::json!({
                "name": name,
                "ok": pass,
                "detail": detail,
            }))
            .collect::<Vec<_>>(),
    });
    emit_value(cfg, &report)?;
    if all_ok {
        Ok(())
    } else {
        Err(CliError::Generic("one or more checks failed".into()))
    }
}

fn locate_daemon_binary() -> Result<PathBuf, CliError> {
    // 1. Sibling of the current `onesync` CLI binary.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join("onesyncd");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    // 2. $PATH lookup.
    if let Ok(path) = std::env::var("PATH") {
        for entry in path.split(':') {
            let candidate = PathBuf::from(entry).join("onesyncd");
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Err(CliError::Generic(
        "onesyncd binary not found alongside the CLI or in $PATH; build it first with `cargo build -p onesync-daemon --release`".into(),
    ))
}

fn run_launchctl(args: &[String]) -> Result<(), CliError> {
    let out = Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|e| CliError::Generic(format!("launchctl: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        return Err(CliError::Generic(format!(
            "launchctl {} failed: {stdout}{stderr}",
            args.first().map_or("", String::as_str)
        )));
    }
    Ok(())
}

async fn wait_for_daemon() -> Result<(), CliError> {
    let socket = default_socket_path();
    let deadline = Instant::now() + Duration::from_secs(PING_TIMEOUT_S);
    while Instant::now() < deadline {
        if let Ok(mut client) = RpcClient::connect(&socket).await {
            if client
                .call::<Value>("health.ping", Value::Null)
                .await
                .is_ok()
            {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(CliError::DaemonNotRunning(format!(
        "daemon did not respond within {PING_TIMEOUT_S}s"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_plist_contains_label() {
        let paths = Paths {
            state_dir: PathBuf::from("/Users/alice/Library/Application Support/onesync"),
            log_dir: PathBuf::from("/Users/alice/Library/Logs/onesync"),
            plist: PathBuf::from("/Users/alice/Library/LaunchAgents/dev.onesync.daemon.plist"),
            daemon_binary: PathBuf::from(
                "/Users/alice/Library/Application Support/onesync/bin/onesyncd",
            ),
        };
        let body = render_plist(&paths);
        assert!(body.contains("<string>dev.onesync.daemon</string>"));
        assert!(body.contains("/Users/alice"));
        assert!(body.contains("RunAtLoad"));
    }

    #[test]
    fn render_plist_is_valid_xml_header() {
        let paths = Paths {
            state_dir: PathBuf::from("/tmp/s"),
            log_dir: PathBuf::from("/tmp/l"),
            plist: PathBuf::from("/tmp/p"),
            daemon_binary: PathBuf::from("/tmp/d"),
        };
        let body = render_plist(&paths);
        assert!(body.starts_with("<?xml version=\"1.0\""));
        assert!(body.contains("<plist version=\"1.0\">"));
        assert!(body.trim_end().ends_with("</plist>"));
    }
}
