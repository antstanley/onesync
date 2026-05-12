//! `onesyncd` — the onesync background daemon.
//!
//! Startup sequence:
//! 1. Parse CLI arguments.
//! 2. Create required directories (`state`, `runtime`, `log`).
//! 3. Acquire the advisory lock (`runtime/onesync.lock`).
//! 4. Initialise structured logging.
//! 5. Start the Tokio multi-thread runtime (capped at [`MAX_RUNTIME_WORKERS`]).
//! 6. (Tasks 10–16) Start IPC server, engine workers, etc.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use clap::Parser;
use onesync_core::limits::max_runtime_workers;
use onesync_daemon::{ipc, lock, logging, methods, shutdown, startup, wiring};

/// Onesync background daemon.
#[derive(Debug, Parser)]
#[command(name = "onesyncd", version, about)]
struct Args {
    /// Override the state directory (`SQLite` database).
    #[arg(long, value_name = "DIR")]
    state_dir: Option<std::path::PathBuf>,

    /// Override the runtime directory (socket + lock file).
    #[arg(long, value_name = "DIR")]
    runtime_dir: Option<std::path::PathBuf>,

    /// Override the log directory.
    #[arg(long, value_name = "DIR")]
    log_dir: Option<std::path::PathBuf>,

    /// Launched by launchd (adjusts log format to work with `os_log`).
    #[arg(long)]
    launchd: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Step 1: Resolve and create directories.
    let dirs = startup::DaemonDirs::resolve(
        args.state_dir.as_deref(),
        args.runtime_dir.as_deref(),
        args.log_dir.as_deref(),
    )?;
    dirs.create_all()?;

    // Step 2: Acquire advisory lock.
    let _lock = lock::acquire(&dirs.runtime_dir)?;

    // Step 3: Build the Tokio runtime.
    let workers = max_runtime_workers();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build Tokio runtime: {e}"))?;

    rt.block_on(async_main(args.launchd, dirs))
}

async fn async_main(launchd: bool, dirs: startup::DaemonDirs) -> anyhow::Result<()> {
    use onesync_protocol::enums::LogLevel;

    // Initialise structured logging.
    logging::init(&dirs.log_dir, LogLevel::Info, launchd)?;
    tracing::info!("onesyncd started");

    // Build ports.
    let ports = wiring::build_ports(&dirs.state_dir)?;

    // Build the per-request dispatch context.
    let ctx = methods::DispatchCtx {
        started_at: std::time::Instant::now(),
        state: ports.state.clone(),
        local_fs: ports.local_fs.clone(),
        clock: ports.clock.clone(),
        ids: ports.ids.clone(),
        audit: ports.audit.clone(),
    };

    // Start shutdown signal handler.
    let token = shutdown::ShutdownToken::new();
    shutdown::spawn_signal_handler(token.clone());

    // Start the IPC server. Returns when the shutdown token fires.
    let runtime_dir = dirs.runtime_dir.clone();
    let server_token = token.clone();
    let server_handle =
        tokio::spawn(async move { ipc::server::run(&runtime_dir, server_token, ctx).await });

    // Wait for shutdown then await the server task.
    let mut rx = token.subscribe();
    let _ = rx.recv().await;

    match server_handle.await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::error!(error = %e, "IPC server exited with error"),
        Err(e) => tracing::error!(error = %e, "IPC server task join error"),
    }

    tracing::info!("onesyncd stopping");
    Ok(())
}
