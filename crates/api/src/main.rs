//! `thewiki` binary entrypoint.
//!
//! Parses the CLI, initialises structured logging, and dispatches to the
//! requested subcommand. All real wiring lives in the library; this file is
//! intentionally trivial.

use anyhow::Context;
use clap::Parser;
use thewiki_api::{app, cli, telemetry};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    telemetry::init();

    match cli.command {
        cli::Command::Serve(args) => serve(args).await,
    }
}

async fn serve(args: cli::ServeArgs) -> anyhow::Result<()> {
    let router = app::build();

    let listener = tokio::net::TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("binding TCP listener on {}", args.bind))?;

    let local_addr = listener
        .local_addr()
        .with_context(|| "reading listener local address")?;
    info!(bind = %local_addr, "thewiki listening");

    axum::serve(listener, router)
        .await
        .context("axum server terminated with an error")?;

    Ok(())
}
