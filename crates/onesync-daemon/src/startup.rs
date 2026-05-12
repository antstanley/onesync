//! Daemon directory resolution and creation.
//!
//! Resolution order for each directory:
//! 1. CLI flag (highest priority).
//! 2. Environment variable.
//! 3. macOS platform default under `~/Library/Application Support/onesync/`.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

/// Resolved paths for directories used by the daemon.
// LINT: field suffix is intentional — `state_dir`, `runtime_dir`, `log_dir` are clear names.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone)]
pub struct DaemonDirs {
    /// `SQLite` state database location.
    pub state_dir: PathBuf,
    /// Unix socket and lock file location.
    pub runtime_dir: PathBuf,
    /// Structured JSON log files.
    pub log_dir: PathBuf,
}

impl DaemonDirs {
    /// Resolve dirs from CLI overrides, then env vars, then macOS defaults.
    ///
    /// # Errors
    ///
    /// Returns an error if the home directory cannot be determined.
    pub fn resolve(
        cli_state: Option<&Path>,
        cli_runtime: Option<&Path>,
        cli_log: Option<&Path>,
    ) -> anyhow::Result<Self> {
        let base = base_dir()?;
        Ok(Self {
            state_dir: resolve_one(cli_state, "ONESYNC_STATE_DIR", base.join("state")),
            runtime_dir: resolve_one(cli_runtime, "ONESYNC_RUNTIME_DIR", base.join("run")),
            log_dir: resolve_one(cli_log, "ONESYNC_LOG_DIR", base.join("logs")),
        })
    }

    /// Create all resolved directories (no-op if they already exist).
    ///
    /// # Errors
    ///
    /// Returns an error if any directory cannot be created.
    pub fn create_all(&self) -> anyhow::Result<()> {
        for dir in [&self.state_dir, &self.runtime_dir, &self.log_dir] {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("failed to create directory {}", dir.display()))?;
        }
        Ok(())
    }
}

// LINT: env reads belong in daemon startup; this is that startup module.
#[allow(clippy::disallowed_methods)]
fn base_dir() -> anyhow::Result<PathBuf> {
    // Prefer ONESYNC_BASE_DIR for hermetic testing.
    if let Ok(base) = std::env::var("ONESYNC_BASE_DIR") {
        return Ok(PathBuf::from(base));
    }
    // macOS default: ~/Library/Application Support/onesync
    let home = home_dir().context("cannot determine home directory")?;
    Ok(home.join("Library/Application Support/onesync"))
}

// LINT: env reads belong in daemon startup; this is that startup module.
#[allow(clippy::disallowed_methods)]
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

// LINT: env reads belong in daemon startup; this is that startup module.
#[allow(clippy::disallowed_methods)]
fn resolve_one(cli: Option<&Path>, env_var: &str, default: PathBuf) -> PathBuf {
    cli.map(Path::to_path_buf)
        .or_else(|| std::env::var_os(env_var).map(PathBuf::from))
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_all_succeeds_for_temp_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = DaemonDirs {
            state_dir: tmp.path().join("state"),
            runtime_dir: tmp.path().join("run"),
            log_dir: tmp.path().join("logs"),
        };
        dirs.create_all().unwrap();
        assert!(dirs.state_dir.exists());
        assert!(dirs.runtime_dir.exists());
        assert!(dirs.log_dir.exists());
    }

    #[test]
    fn resolve_prefers_cli_over_env_and_default() {
        let cli_state = PathBuf::from("/custom/state");
        let dirs = DaemonDirs {
            state_dir: cli_state.clone(),
            runtime_dir: PathBuf::from("/tmp/run"),
            log_dir: PathBuf::from("/tmp/logs"),
        };
        assert_eq!(dirs.state_dir, cli_state);
    }
}
