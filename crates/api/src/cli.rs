//! CLI surface for the `thewiki` binary.
//!
//! Subcommands are listed in [`Command`]. The skeleton ships only `serve`;
//! `config` and `migrate` are sketched so #8 (config loading) and the storage
//! work can add concrete behaviour without churning the CLI shape.

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
    // TODO(#8): `Config { Show, Validate }` once the config loader lands.
    // TODO(storage): `Migrate { Run, Status }` once `thewiki-storage` is wired.
}

/// Arguments for the `serve` subcommand.
///
/// Bind address is read here as a fallback for the env var; once #8 lands the
/// real source of truth is `Config`, and this struct can fall away or shrink.
#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    /// Address to bind the HTTP listener to.
    ///
    /// TODO(#8): replace with a value plumbed through `Config`.
    #[arg(long, env = "THEWIKI_BIND", default_value = "0.0.0.0:8080")]
    pub bind: String,
}
