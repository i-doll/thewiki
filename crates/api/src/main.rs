//! `thewiki` binary entrypoint.
//!
//! Parses the CLI, initialises structured logging, and dispatches to the
//! requested subcommand. All real wiring lives in the library; this file is
//! intentionally trivial.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use thewiki_api::auth::password::Argon2Hasher;
use thewiki_api::auth::state::AuthState;
use thewiki_api::{
    app,
    cli::{self, ConfigCommand},
    config::Config,
    telemetry,
};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
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

    // Open storage + apply migrations. The pool stays alive for the lifetime
    // of the process.
    let storage = SqliteStorage::new(
        &config.database.url,
        SqliteOptions {
            max_connections: config.database.max_connections,
            acquire_timeout: Duration::from_secs(config.database.acquire_timeout_secs),
            foreign_keys: true,
        },
    )
    .await
    .with_context(|| format!("opening storage at {}", config.database.url))?;

    let hasher = Arc::new(Argon2Hasher::new(config.auth.argon2).context("building argon2 hasher")?);
    let session_ttl = Duration::from_secs(u64::from(config.auth.session_ttl_hours) * 3600);
    let secure_cookies = !args.insecure_cookie;
    if !secure_cookies {
        tracing::warn!(
            "running with --insecure-cookie: session cookies omit the Secure flag — \
             local dev only, do not use over plain HTTP in production"
        );
    }
    let auth_state = AuthState::new(storage.clone(), hasher, session_ttl, secure_cookies);
    let app_state = thewiki_api::state::AppState::new(storage);

    let router = app::build_full(app_state, auth_state, config.server.serve_frontend);

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
