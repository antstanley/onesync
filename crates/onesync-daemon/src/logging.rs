//! Structured logging initialisation.
//!
//! Writes compact JSON to `<log_dir>/onesyncd.jsonl` and simultaneously
//! emits human-readable output to stderr (for `launchd` / terminal use).
//! Rotates the JSONL file once it exceeds [`LOG_ROTATE_BYTES`] bytes.
//!
//! Call [`init`] once at daemon startup, after the log directory exists.

// LINT: all items in this module are called from async_main (Task 10 wiring).
#![allow(dead_code)]

use std::fs::{File, OpenOptions};
use std::path::Path;

use anyhow::Context as _;
use onesync_core::limits::{LOG_RETAIN_FILES, LOG_ROTATE_BYTES};
use onesync_protocol::enums::LogLevel;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer};

/// The base name of the structured log file.
const LOG_FILE: &str = "onesyncd.jsonl";

/// Initialise the global `tracing` subscriber.
///
/// - A JSON layer writes to `<log_dir>/onesyncd.jsonl`.
/// - A human-readable fmt layer writes to stderr.
/// - `RUST_LOG` overrides `log_level`; absent, `log_level` is used as the default.
/// - If the log file already exceeds [`LOG_ROTATE_BYTES`], it is rotated before
///   opening the new file.
///
/// # Errors
///
/// Returns an error if the log file cannot be opened or the subscriber cannot
/// be installed (e.g., called twice).
pub fn init(log_dir: &Path, log_level: LogLevel, launchd: bool) -> anyhow::Result<()> {
    rotate_if_needed(log_dir).context("log rotation")?;

    let log_path = log_dir.join(LOG_FILE);
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open log file {}", log_path.display()))?;

    let level_str = log_level_str(log_level);
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level_str));

    // JSON layer → file
    let json_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(file)
        .with_filter(env_filter.clone());

    if launchd {
        // Under launchd the kernel captures stderr; use compact text without ANSI.
        let text_layer = tracing_subscriber::fmt::layer()
            .compact()
            .with_ansi(false)
            .with_writer(std::io::stderr)
            .with_filter(env_filter);

        tracing_subscriber::registry()
            .with(json_layer)
            .with(text_layer)
            .try_init()
            .context("install tracing subscriber")?;
    } else {
        let text_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_filter(env_filter);

        tracing_subscriber::registry()
            .with(json_layer)
            .with(text_layer)
            .try_init()
            .context("install tracing subscriber")?;
    }

    Ok(())
}

/// Rotate `onesyncd.jsonl` → `onesyncd.1.jsonl` … `onesyncd.N.jsonl` if the
/// current file exceeds [`LOG_ROTATE_BYTES`].  Oldest rotated file is deleted
/// once [`LOG_RETAIN_FILES`] are already present.
fn rotate_if_needed(log_dir: &Path) -> anyhow::Result<()> {
    let current = log_dir.join(LOG_FILE);
    if !current.exists() {
        return Ok(());
    }

    let size = std::fs::metadata(&current).context("stat log file")?.len();
    if size < LOG_ROTATE_BYTES {
        return Ok(());
    }

    // Shift existing rotated files: N-1 → N (drop if > RETAIN).
    for n in (1..=LOG_RETAIN_FILES).rev() {
        let src = rotated_path(log_dir, n - 1);
        let dst = rotated_path(log_dir, n);
        if n > LOG_RETAIN_FILES {
            // Drop the oldest.
            if dst.exists() {
                std::fs::remove_file(&dst).context("remove old log")?;
            }
            continue;
        }
        if src.exists() {
            std::fs::rename(&src, &dst).context("shift log file")?;
        }
    }

    // Rename current → .1
    let slot1 = rotated_path(log_dir, 1);
    std::fs::rename(&current, slot1).context("rotate current log")?;

    // Truncate (create fresh) the current file so we start clean.
    File::create(&current).context("create fresh log file")?;
    Ok(())
}

fn rotated_path(log_dir: &Path, n: u32) -> std::path::PathBuf {
    if n == 0 {
        log_dir.join(LOG_FILE)
    } else {
        log_dir.join(format!("onesyncd.{n}.jsonl"))
    }
}

const fn log_level_str(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
        LogLevel::Trace => "trace",
    }
}
