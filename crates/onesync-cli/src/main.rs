//! `onesync` CLI entry point.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use clap::Parser as _;
use std::process::ExitCode;

mod cli;
mod commands;
mod error;
mod exit_codes;
mod output;
mod rpc;

use cli::Cli;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to build runtime: {e}");
            return ExitCode::from(1);
        }
    };
    match rt.block_on(commands::run(cli)) {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(exit_codes::exit_code_for(&e))
        }
    }
}
