//! `xtask migrate` — database migration commands.
//!
//! `run` and `add` are real; `status` and `revert` are stubs until the second
//! backend lands in M1 (issues #24, #25). Keeping them in the surface now
//! keeps the CLI shape stable.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::Subcommand;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqlitePoolOptions;
use time::OffsetDateTime;
use time::format_description::FormatItem;
use time::macros::format_description;

/// Filenames look like `20251121093015_<slug>.sql`. We also keep the special
/// all-zeros prefix as a fast-path for the inaugural migration.
const TIMESTAMP_FORMAT: &[FormatItem<'_>] =
    format_description!("[year][month][day][hour][minute][second]");

#[derive(Debug, Subcommand)]
pub enum MigrateCommand {
    /// Create a new migration file in `migrations/`.
    Add {
        /// Short snake_case slug describing the migration (e.g. `add_tags_table`).
        slug: String,
        /// Also create a Postgres-flavour stub under `migrations/postgres/`.
        #[arg(long)]
        postgres: bool,
    },
    /// Apply all pending migrations against the configured database.
    Run {
        /// Database URL. Defaults to `$DATABASE_URL`. Required if neither is set.
        #[arg(long, env = "DATABASE_URL")]
        database_url: Option<String>,
    },
    /// (Stub, lands in M1) Print which migrations have been applied.
    Status,
    /// (Stub, lands in M1) Revert the most recent migration.
    Revert,
}

pub fn run(cmd: MigrateCommand) -> Result<()> {
    match cmd {
        MigrateCommand::Add { slug, postgres } => add_migration(&slug, postgres),
        MigrateCommand::Run { database_url } => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("build tokio runtime")?;
            runtime.block_on(run_migrations(database_url))
        }
        MigrateCommand::Status => {
            println!("xtask migrate status: TODO — lands in M1 (#24/#25).");
            Ok(())
        }
        MigrateCommand::Revert => {
            println!("xtask migrate revert: TODO — lands in M1 (#24/#25).");
            println!("(Migrations are forward-only; see migrations/README.md.)");
            Ok(())
        }
    }
}

/// Resolve the `migrations/` directory relative to the workspace root.
///
/// `xtask` is always invoked through `cargo run -p xtask`, which runs the
/// binary from the workspace root, but we resolve via `CARGO_MANIFEST_DIR`
/// to be robust against direct invocations from elsewhere.
fn migrations_dir() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(|workspace| workspace.join("migrations"))
        .unwrap_or_else(|| PathBuf::from("migrations"))
}

fn add_migration(slug: &str, postgres: bool) -> Result<()> {
    validate_slug(slug)?;

    let dir = migrations_dir();
    if !dir.is_dir() {
        bail!(
            "migrations directory not found at {} — run from the workspace root",
            dir.display()
        );
    }

    let timestamp = OffsetDateTime::now_utc()
        .format(TIMESTAMP_FORMAT)
        .context("format migration timestamp")?;
    let filename = format!("{timestamp}_{slug}.sql");
    let path = dir.join(&filename);
    if path.exists() {
        bail!("migration already exists: {}", path.display());
    }
    std::fs::write(&path, header(slug))
        .with_context(|| format!("create migration {}", path.display()))?;
    println!("created {}", path.display());

    if postgres {
        let pg_dir = dir.join("postgres");
        std::fs::create_dir_all(&pg_dir).with_context(|| format!("ensure {}", pg_dir.display()))?;
        let pg_path = pg_dir.join(&filename);
        if pg_path.exists() {
            bail!("postgres variant already exists: {}", pg_path.display());
        }
        std::fs::write(&pg_path, header(slug))
            .with_context(|| format!("create postgres variant {}", pg_path.display()))?;
        println!("created {}", pg_path.display());
    }

    Ok(())
}

fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() {
        bail!("slug must not be empty");
    }
    if let Some(bad) = slug
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_'))
    {
        bail!("slug must be snake_case ASCII (got {bad:?})");
    }
    Ok(())
}

fn header(slug: &str) -> String {
    format!("-- {slug}\n--\n-- TODO: describe the migration intent here.\n\n")
}

async fn run_migrations(database_url: Option<String>) -> Result<()> {
    let url = database_url.ok_or_else(|| {
        anyhow!(
            "DATABASE_URL is not set; pass --database-url or define it in the environment / .env"
        )
    })?;

    // SQLite is the only backend at M0. Connecting via the generic
    // `AnyPool` would force us to enable extra drivers we don't need yet;
    // when libsql/Postgres land (M1) this dispatch grows.
    if !is_sqlite_url(&url) {
        bail!("only sqlite:// URLs are supported at M0 (got {url:?}); libsql/postgres land in M1");
    }

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .with_context(|| format!("connect to {url}"))?;

    let dir = migrations_dir();
    let migrator = Migrator::new(dir.as_path())
        .await
        .with_context(|| format!("load migrations from {}", dir.display()))?;
    migrator
        .run(&pool)
        .await
        .context("apply pending migrations")?;

    println!("migrations applied against {url}");
    Ok(())
}

fn is_sqlite_url(url: &str) -> bool {
    url.starts_with("sqlite:") || url.starts_with("sqlite::")
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn slug_accepts_snake_case() {
        validate_slug("init").expect("simple");
        validate_slug("add_tags_table").expect("snake case");
        validate_slug("v2_widgets").expect("digits ok");
    }

    #[test]
    fn slug_rejects_hyphens_and_spaces() {
        assert!(validate_slug("").is_err());
        assert!(validate_slug("add-tags").is_err());
        assert!(validate_slug("add tags").is_err());
    }

    #[test]
    fn sqlite_url_detection() {
        assert!(is_sqlite_url("sqlite::memory:"));
        assert!(is_sqlite_url("sqlite:./dev.db"));
        assert!(!is_sqlite_url("postgres://localhost/thewiki"));
    }
}
