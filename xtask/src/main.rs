//! Repo-local automation entrypoint.
//!
//! Subcommands are dispatched via `clap`. `migrate` manages schema migrations;
//! `audit-log-prune` enforces the `audit_log.retention_days` policy outside the
//! server process. Future tasks (codegen, release helpers, etc.) attach here.

use anyhow::Result;
use clap::Parser;

mod audit_log_prune;
mod migrate;

#[derive(Debug, Parser)]
#[command(name = "xtask", about = "Repo-local automation for thewiki.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, clap::Subcommand)]
enum Command {
    /// Manage database migrations.
    #[command(subcommand)]
    Migrate(migrate::MigrateCommand),
    /// Delete audit-log rows older than the configured retention window.
    AuditLogPrune(audit_log_prune::AuditLogPruneArgs),
}

fn main() -> Result<()> {
    // Best-effort: pull `DATABASE_URL` (and friends) from a local `.env`.
    // Missing file is fine; anything else is surfaced via the migrate command.
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();
    match cli.command {
        Command::Migrate(cmd) => migrate::run(cmd),
        Command::AuditLogPrune(args) => audit_log_prune::run(args),
    }
}
