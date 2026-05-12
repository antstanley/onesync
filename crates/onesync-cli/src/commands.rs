//! Command dispatch. Each command is a thin wrapper that constructs the right
//! JSON-RPC call and hands the result to the output formatter.

use serde_json::{Value, json};

use crate::cli::{
    AccountCmd, Cli, Command, ConfigCmd, ConflictsCmd, LogsCmd, PairCmd, ServiceCmd, StateCmd,
};
use crate::error::CliError;
use crate::output::{OutputCfg, emit_value};
use crate::rpc::{RpcClient, default_socket_path};

const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn run(cli: Cli) -> Result<(), CliError> {
    let cfg = OutputCfg::from_flags(cli.json, cli.no_color);
    let socket = cli.socket.clone().unwrap_or_else(default_socket_path);

    match cli.command.unwrap_or(Command::Status) {
        Command::Version => {
            if cfg.json {
                println!("{}", json!({ "version": CLI_VERSION }));
            } else {
                println!("onesync {CLI_VERSION}");
            }
            Ok(())
        }
        Command::Status => status(&socket, cfg).await,
        Command::Account { cmd } => account(&socket, cfg, cmd).await,
        Command::Pair { cmd } => pair(&socket, cfg, cmd).await,
        Command::Conflicts { cmd } => conflicts(&socket, cfg, cmd).await,
        Command::Logs { cmd } => logs(&socket, cfg, cmd).await,
        Command::State { cmd } => state(&socket, cfg, cmd).await,
        Command::Config { cmd } => config(&socket, cfg, cmd).await,
        Command::Service { cmd } => service(cfg, cmd),
    }
}

async fn client(socket: &std::path::Path) -> Result<RpcClient, CliError> {
    RpcClient::connect(socket).await
}

async fn rpc(socket: &std::path::Path, method: &str, params: Value) -> Result<Value, CliError> {
    let mut c = client(socket).await?;
    c.call::<Value>(method, params).await
}

async fn status(socket: &std::path::Path, cfg: OutputCfg) -> Result<(), CliError> {
    let v = rpc(socket, "health.diagnostics", Value::Null).await?;
    emit_value(cfg, &v)
}

async fn account(
    socket: &std::path::Path,
    cfg: OutputCfg,
    cmd: AccountCmd,
) -> Result<(), CliError> {
    let v = match cmd {
        AccountCmd::Login { client_id } => {
            let params = client_id.map_or(Value::Null, |c| json!({ "client_id": c }));
            rpc(socket, "account.login.begin", params).await?
        }
        AccountCmd::List => rpc(socket, "account.list", Value::Null).await?,
        AccountCmd::Remove {
            account_id,
            cascade_pairs,
            yes: _,
        } => {
            rpc(
                socket,
                "account.remove",
                json!({ "account_id": account_id, "cascade_pairs": cascade_pairs }),
            )
            .await?
        }
    };
    emit_value(cfg, &v)
}

async fn pair(socket: &std::path::Path, cfg: OutputCfg, cmd: PairCmd) -> Result<(), CliError> {
    let v = match cmd {
        PairCmd::Add {
            account,
            local,
            remote,
            name,
        } => {
            rpc(
                socket,
                "pair.add",
                json!({
                    "account_id": account,
                    "local_path": local.to_string_lossy(),
                    "remote_path": remote,
                    "display_name": name,
                }),
            )
            .await?
        }
        PairCmd::List {
            account,
            include_removed,
        } => {
            rpc(
                socket,
                "pair.list",
                json!({ "account_id": account, "include_removed": include_removed }),
            )
            .await?
        }
        PairCmd::Show { pair_id } => {
            rpc(socket, "pair.status", json!({ "pair_id": pair_id })).await?
        }
        PairCmd::Pause { pair_id } => {
            rpc(socket, "pair.pause", json!({ "pair_id": pair_id })).await?
        }
        PairCmd::Resume { pair_id } => {
            rpc(socket, "pair.resume", json!({ "pair_id": pair_id })).await?
        }
        PairCmd::Remove {
            pair_id,
            delete_local,
            delete_remote,
            yes: _,
        } => {
            rpc(
                socket,
                "pair.remove",
                json!({
                    "pair_id": pair_id,
                    "delete_local": delete_local,
                    "delete_remote": delete_remote,
                }),
            )
            .await?
        }
        PairCmd::Sync { pair_id, full } => {
            rpc(
                socket,
                "pair.force_sync",
                json!({ "pair_id": pair_id, "full_scan": full }),
            )
            .await?
        }
    };
    emit_value(cfg, &v)
}

async fn conflicts(
    socket: &std::path::Path,
    cfg: OutputCfg,
    cmd: ConflictsCmd,
) -> Result<(), CliError> {
    let v = match cmd {
        ConflictsCmd::List { pair, all } => {
            rpc(
                socket,
                "conflict.list",
                json!({ "pair_id": pair, "include_resolved": all }),
            )
            .await?
        }
        ConflictsCmd::Show { conflict_id } => {
            rpc(
                socket,
                "conflict.get",
                json!({ "conflict_id": conflict_id }),
            )
            .await?
        }
        ConflictsCmd::Resolve {
            conflict_id,
            pick,
            discard_loser,
            note,
        } => {
            rpc(
                socket,
                "conflict.resolve",
                json!({
                    "conflict_id": conflict_id,
                    "pick": pick,
                    "keep_loser": !discard_loser,
                    "note": note,
                }),
            )
            .await?
        }
    };
    emit_value(cfg, &v)
}

async fn logs(socket: &std::path::Path, cfg: OutputCfg, cmd: LogsCmd) -> Result<(), CliError> {
    let v = match cmd {
        LogsCmd::Tail { level, kind } => {
            // One-shot fetch of recent events; true streaming via subscription
            // is a future enhancement.
            rpc(
                socket,
                "audit.tail",
                json!({ "level": level, "kind_prefix": kind }),
            )
            .await?
        }
        LogsCmd::Search {
            since,
            until,
            pair,
            level,
        } => {
            rpc(
                socket,
                "audit.search",
                json!({
                    "from_ts": since,
                    "to_ts": until,
                    "pair_id": pair,
                    "level": level,
                    "limit": 1000,
                }),
            )
            .await?
        }
    };
    emit_value(cfg, &v)
}

async fn state(socket: &std::path::Path, cfg: OutputCfg, cmd: StateCmd) -> Result<(), CliError> {
    let v = match cmd {
        StateCmd::Backup { to } => {
            rpc(
                socket,
                "state.backup",
                json!({ "to_path": to.to_string_lossy() }),
            )
            .await?
        }
        StateCmd::Export { to } => {
            rpc(
                socket,
                "state.export",
                json!({ "to_dir": to.to_string_lossy() }),
            )
            .await?
        }
        StateCmd::RepairPerms => rpc(socket, "state.repair.permissions", Value::Null).await?,
        StateCmd::Compact => rpc(socket, "state.compact.now", Value::Null).await?,
    };
    emit_value(cfg, &v)
}

async fn config(socket: &std::path::Path, cfg: OutputCfg, cmd: ConfigCmd) -> Result<(), CliError> {
    let v = match cmd {
        ConfigCmd::Get => rpc(socket, "config.get", Value::Null).await?,
        ConfigCmd::Set { key, value } => {
            let parsed: Value = serde_json::from_str(&value).unwrap_or(Value::String(value));
            rpc(socket, "config.set", json!({ key: parsed })).await?
        }
    };
    emit_value(cfg, &v)
}

fn service(_cfg: OutputCfg, _cmd: ServiceCmd) -> Result<(), CliError> {
    // M7 lifecycle implementations replace this stub.
    Err(CliError::Generic(
        "service subcommands are implemented in M7 (installation lifecycle)".into(),
    ))
}
