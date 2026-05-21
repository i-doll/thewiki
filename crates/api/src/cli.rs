//! CLI surface for the `thewiki` binary.
//!
//! Subcommands are listed in [`Command`]. `serve` boots the HTTP server;
//! `config check` and `config print` give operators a way to validate and
//! inspect a config file before booting (#8).
//!
//! Storage migration subcommands land alongside the persistence work.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Top-level CLI parser.
#[derive(Debug, Parser)]
#[command(
    name = "thewiki",
    version,
    about = "thewiki — self-hosted wiki server",
    long_about = None,
    propagate_version = true,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// Subcommands understood by the binary.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Start the HTTP server.
    Serve(ServeArgs),
    /// Inspect or validate configuration without booting the server.
    #[command(subcommand)]
    Config(ConfigCommand),
    // TODO(storage): `Migrate { Run, Status }` once `thewiki-storage` is wired.
}

/// Arguments for the `serve` subcommand.
///
/// `--config` / `-c` (or `THEWIKI_CONFIG_PATH`) selects an optional TOML file.
/// Everything else flows through the layered config loader.
#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    /// Path to a `thewiki.toml` configuration file.
    ///
    /// When omitted, the server boots from built-in defaults overlaid with
    /// any `THEWIKI_*` environment variables.
    #[arg(short = 'c', long = "config", env = "THEWIKI_CONFIG_PATH")]
    pub config: Option<PathBuf>,
}

/// `config` subcommands. These are debug/operator aids — they never bind a
/// listener.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Load and validate configuration. Exits non-zero on any error.
    Check {
        /// Path to a `thewiki.toml` configuration file.
        #[arg(short = 'f', long = "file", env = "THEWIKI_CONFIG_PATH")]
        file: Option<PathBuf>,
    },
    /// Print the fully resolved configuration (defaults < file < env). Useful
    /// for debugging why a key is or isn't what you expect.
    Print {
        /// Path to a `thewiki.toml` configuration file.
        #[arg(short = 'f', long = "file", env = "THEWIKI_CONFIG_PATH")]
        file: Option<PathBuf>,
        /// Print as JSON instead of TOML.
        #[arg(long)]
        json: bool,
    },
}
