//! Output formatters.

#![allow(dead_code)]
// LINT: `emit` / `emit_each` are part of the public output surface for command modules
//       that render typed entities; the v1 CLI delegates to `emit_value` for raw JSON,
//       but the typed helpers are exposed for future use and tested through doctests.

use serde::Serialize;

use crate::error::CliError;

/// Output configuration carried in command handlers.
#[derive(Debug, Clone, Copy)]
pub struct OutputCfg {
    pub json: bool,
    /// Honour ANSI colour. The v1 CLI doesn't colourise output yet; the flag is
    /// captured so typed-entity formatters added later can read it without a
    /// signature change.
    pub color: bool,
}

impl OutputCfg {
    /// Build from the global CLI flags + `NO_COLOR` env.
    #[must_use]
    pub fn from_flags(json: bool, no_color: bool) -> Self {
        let no_color_env = std::env::var_os("NO_COLOR").is_some();
        Self {
            json,
            color: !no_color && !no_color_env,
        }
    }
}

/// Print an entity as either JSON or a human-readable line, honouring `cfg.json`.
pub fn emit<T: Serialize + std::fmt::Display>(cfg: OutputCfg, value: &T) -> Result<(), CliError> {
    if cfg.json {
        let json = serde_json::to_string(value)?;
        println!("{json}");
    } else {
        println!("{value}");
    }
    Ok(())
}

/// Print an arbitrary JSON value (used for endpoints whose results don't have
/// dedicated typed entities). In JSON mode, emit it verbatim; in human mode,
/// pretty-print.
pub fn emit_value(cfg: OutputCfg, value: &serde_json::Value) -> Result<(), CliError> {
    if cfg.json {
        println!("{}", serde_json::to_string(value)?);
    } else {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

/// Print a slice of entities one-per-line (JSONL in `--json` mode).
pub fn emit_each<T: Serialize + std::fmt::Display>(
    cfg: OutputCfg,
    items: &[T],
) -> Result<(), CliError> {
    for item in items {
        emit(cfg, item)?;
    }
    Ok(())
}
