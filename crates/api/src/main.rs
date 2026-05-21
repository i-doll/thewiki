//! `thewiki` binary entrypoint.
//!
//! Parses the CLI, initialises structured logging, and dispatches to the
//! requested subcommand. All real wiring lives in the library; this file is
//! intentionally trivial.

use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;
use thewiki_api::{
    app,
    cli::{self, ConfigCommand},
    config::Config,
    telemetry,
};
use tracing::{error, info};

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    let cli = cli::Cli::parse();
    telemetry::init();

    match cli.command {
        cli::Command::Serve(args) => serve(args).await.map(|()| ExitCode::SUCCESS),
        cli::Command::Config(cmd) => Ok(run_config(cmd)),
    }
}

async fn serve(args: cli::ServeArgs) -> anyhow::Result<()> {
    let config = Config::load(args.config.as_deref()).context("loading configuration")?;
    config.validate().context("validating configuration")?;

    let router = app::build();

    let listener = tokio::net::TcpListener::bind(&config.server.bind)
        .await
        .with_context(|| format!("binding TCP listener on {}", config.server.bind))?;

    let local_addr = listener
        .local_addr()
        .with_context(|| "reading listener local address")?;
    info!(bind = %local_addr, "thewiki listening");

    axum::serve(listener, router)
        .await
        .context("axum server terminated with an error")?;

    Ok(())
}

/// Run a `config` subcommand. Returns the process exit code rather than
/// bubbling errors through anyhow so a validation failure surfaces as a clean
/// non-zero exit (the user-visible behaviour the smoke tests rely on).
fn run_config(cmd: ConfigCommand) -> ExitCode {
    match cmd {
        ConfigCommand::Check { file } => match Config::load(file.as_deref()) {
            Ok(cfg) => match cfg.validate() {
                Ok(()) => {
                    println!("config OK");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    error!(error = %e, "configuration is invalid");
                    eprintln!("config check failed: {e}");
                    ExitCode::FAILURE
                }
            },
            Err(e) => {
                error!(error = %e, "failed to load configuration");
                eprintln!("config check failed: {e}");
                ExitCode::FAILURE
            }
        },
        ConfigCommand::Print { file, json } => match Config::load(file.as_deref()) {
            Ok(cfg) => match render_config(&cfg, json) {
                Ok(out) => {
                    println!("{out}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("failed to serialise config: {e}");
                    ExitCode::FAILURE
                }
            },
            Err(e) => {
                eprintln!("failed to load configuration: {e}");
                ExitCode::FAILURE
            }
        },
    }
}

fn render_config(cfg: &Config, json: bool) -> anyhow::Result<String> {
    if json {
        serde_json::to_string_pretty(cfg).context("serialising config as JSON")
    } else {
        toml::to_string_pretty(cfg).context("serialising config as TOML")
    }
}
