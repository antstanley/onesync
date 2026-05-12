//! Clap-derive CLI surface. Mirrors `docs/spec/07-cli-and-ipc.md` §CLI surface.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "onesync",
    version,
    about = "macOS daemon and CLI for two-way OneDrive sync",
    propagate_version = true
)]
pub struct Cli {
    /// Emit results as JSON (JSONL for streaming subscriptions).
    #[arg(long, global = true)]
    pub json: bool,

    /// Disable ANSI colour in human output. Also disabled when `NO_COLOR` is set.
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Override the daemon socket path (defaults to `${TMPDIR}onesync/onesync.sock`).
    #[arg(long, global = true)]
    pub socket: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Daemon + pair overview (default when no subcommand given).
    Status,

    /// Account management.
    Account {
        #[command(subcommand)]
        cmd: AccountCmd,
    },

    /// Pair management.
    Pair {
        #[command(subcommand)]
        cmd: PairCmd,
    },

    /// Conflict inspection and resolution.
    Conflicts {
        #[command(subcommand)]
        cmd: ConflictsCmd,
    },

    /// Audit-log tail and search.
    Logs {
        #[command(subcommand)]
        cmd: LogsCmd,
    },

    /// State-store backup / export / repair.
    State {
        #[command(subcommand)]
        cmd: StateCmd,
    },

    /// Instance config.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },

    /// `LaunchAgent` lifecycle (real impls in M7; stubs here).
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
    },

    /// Print the CLI version.
    Version,
}

#[derive(Debug, Subcommand)]
pub enum AccountCmd {
    /// Begin an OAuth login.
    Login {
        /// Optional Azure AD `client_id`.
        #[arg(long)]
        client_id: Option<String>,
    },
    /// List accounts.
    List,
    /// Remove an account.
    Remove {
        account_id: String,
        /// Cascade-remove pairs that reference this account.
        #[arg(long)]
        cascade_pairs: bool,
        /// Skip confirmation prompts.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum PairCmd {
    /// Register a new pair.
    Add {
        #[arg(long)]
        account: String,
        #[arg(long)]
        local: std::path::PathBuf,
        #[arg(long)]
        remote: String,
        #[arg(long)]
        name: Option<String>,
    },
    /// List pairs.
    List {
        #[arg(long)]
        account: Option<String>,
        #[arg(long)]
        include_removed: bool,
    },
    /// Show pair status detail.
    Show { pair_id: String },
    /// Pause sync on a pair.
    Pause { pair_id: String },
    /// Resume sync on a pair.
    Resume { pair_id: String },
    /// Remove a pair.
    Remove {
        pair_id: String,
        #[arg(long)]
        delete_local: bool,
        #[arg(long)]
        delete_remote: bool,
        #[arg(long)]
        yes: bool,
    },
    /// Force an immediate sync cycle.
    Sync {
        pair_id: String,
        #[arg(long)]
        full: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConflictsCmd {
    /// List conflicts.
    List {
        #[arg(long)]
        pair: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Show a single conflict.
    Show { conflict_id: String },
    /// Resolve a conflict.
    Resolve {
        conflict_id: String,
        /// Which side wins.
        #[arg(long, value_parser = ["local", "remote"])]
        pick: String,
        /// Discard the loser instead of keeping it under the renamed path.
        #[arg(long)]
        discard_loser: bool,
        #[arg(long)]
        note: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum LogsCmd {
    /// Tail audit events.
    Tail {
        #[arg(long)]
        level: Option<String>,
        #[arg(long)]
        kind: Option<String>,
    },
    /// Search audit events.
    Search {
        #[arg(long)]
        since: String,
        #[arg(long)]
        until: String,
        #[arg(long)]
        pair: Option<String>,
        #[arg(long)]
        level: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum StateCmd {
    /// Take a consistent backup of the state database.
    Backup {
        #[arg(long)]
        to: std::path::PathBuf,
    },
    /// Export every table as JSON Lines for support escalation.
    Export {
        #[arg(long)]
        to: std::path::PathBuf,
    },
    /// Re-apply the documented file permissions on state-dir contents.
    RepairPerms,
    /// Run a retention compaction pass now.
    Compact,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Print the current instance config.
    Get,
    /// Set a single config field.
    Set { key: String, value: String },
}

#[derive(Debug, Subcommand)]
pub enum ServiceCmd {
    /// Install the daemon as a `LaunchAgent` (M7).
    Install,
    /// Uninstall the `LaunchAgent` (M7).
    Uninstall {
        #[arg(long)]
        purge: bool,
    },
    /// Start the daemon.
    Start,
    /// Stop the daemon.
    Stop,
    /// Restart the daemon.
    Restart,
    /// Run the install-health checklist.
    Doctor,
}
