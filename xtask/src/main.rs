//! Repo-local automation entrypoint.
//!
//! Subcommands are dispatched via `clap`. Today only `migrate` is wired up;
//! future tasks (codegen, release helpers, etc.) attach here.

use anyhow::Result;
use clap::Parser;

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
}

fn main() -> Result<()> {
    // Best-effort: pull `DATABASE_URL` (and friends) from a local `.env`.
    // Missing file is fine; anything else is surfaced via the migrate command.
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();
    match cli.command {
        Command::Migrate(cmd) => migrate::run(cmd),
    }
}
