//! `xtask audit-log-prune` — delete audit-log rows beyond the retention window.
//!
//! The `serve` binary already runs an in-process pruner once an hour, but the
//! retention policy lives in `audit_log.retention_days` so operators who want
//! to drive it from cron (or skip running the server during maintenance) can
//! invoke this subcommand directly. See the `[audit_log]` section of
//! `thewiki.example.toml` for a sample crontab entry.

use anyhow::{Context, Result, anyhow, bail};
use clap::Args;
use std::time::Duration;
use thewiki_storage::repo::AuditLogRepository;
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::{Duration as TimeDuration, OffsetDateTime};

/// Default retention. Mirrors `Config::defaults().audit_log.retention_days`.
const DEFAULT_RETENTION_DAYS: u32 = 365;

#[derive(Debug, Args)]
pub struct AuditLogPruneArgs {
    /// Database URL. Defaults to `$DATABASE_URL`. Required if neither is set.
    #[arg(long, env = "DATABASE_URL")]
    database_url: Option<String>,
    /// Retention window in days. Rows with `created_at` older than `now -
    /// retention_days` are deleted. Defaults to 365 (mirrors `Config`).
    #[arg(long, default_value_t = DEFAULT_RETENTION_DAYS)]
    retention_days: u32,
}

pub fn run(args: AuditLogPruneArgs) -> Result<()> {
    if args.retention_days == 0 {
        bail!("--retention-days must be > 0");
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build tokio runtime")?;
    runtime.block_on(prune(args))
}

async fn prune(args: AuditLogPruneArgs) -> Result<()> {
    let url = args.database_url.ok_or_else(|| {
        anyhow!(
            "DATABASE_URL is not set; pass --database-url or define it in the environment / .env"
        )
    })?;

    if !is_sqlite_url(&url) {
        // The storage layer's other backends (postgres, libsql) also implement
        // `AuditLogRepository::prune_before`, but the binary currently bundles
        // only the SQLite facade. Once #24/#25 land, this dispatch grows.
        bail!("only sqlite:// URLs are supported today (got {url:?}); postgres/libsql land in M1");
    }

    let storage = SqliteStorage::new(
        &url,
        SqliteOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(10),
            foreign_keys: true,
        },
    )
    .await
    .with_context(|| format!("open sqlite storage at {url}"))?;

    let cutoff = OffsetDateTime::now_utc() - TimeDuration::days(i64::from(args.retention_days));
    let pruned = storage
        .audit_log()
        .prune_before(cutoff)
        .await
        .context("prune audit_log rows")?;
    println!(
        "pruned {pruned} audit_log row(s) older than {} days (cutoff: {})",
        args.retention_days,
        cutoff
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| cutoff.to_string()),
    );
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
    fn rejects_unsupported_url() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let err = runtime
            .block_on(prune(AuditLogPruneArgs {
                database_url: Some("postgres://localhost/thewiki".to_string()),
                retention_days: 30,
            }))
            .expect_err("postgres URL must be rejected today");
        assert!(format!("{err}").contains("sqlite"));
    }

    #[test]
    fn rejects_zero_retention() {
        let err = run(AuditLogPruneArgs {
            database_url: Some("sqlite::memory:".to_string()),
            retention_days: 0,
        })
        .expect_err("zero retention rejected");
        // The bail! message includes the literal flag name `--retention-days`.
        assert!(
            format!("{err}").contains("--retention-days"),
            "unexpected error message: {err}",
        );
    }

    #[test]
    fn prunes_old_rows_against_in_memory_db() {
        use serde_json::json;
        use thewiki_core::{PageId, UserId};
        use thewiki_storage::repo::{AuditLogFilter, AuditLogRepository, NewAuditLogEntry};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let storage = SqliteStorage::new(
                "sqlite::memory:",
                SqliteOptions {
                    max_connections: 1,
                    acquire_timeout: Duration::from_secs(5),
                    foreign_keys: true,
                },
            )
            .await
            .expect("open in-memory sqlite");

            // Insert one fresh row directly; prune_before with a future cutoff
            // wipes it, while a far-past cutoff leaves it alone.
            storage
                .audit_log()
                .create(NewAuditLogEntry {
                    actor_id: UserId::new(),
                    actor_username: "alice".to_string(),
                    action: "page.create".to_string(),
                    target_kind: "page".to_string(),
                    target_id: PageId::new().into_uuid(),
                    target_label: Some("Main/home".to_string()),
                    metadata: json!({}),
                })
                .await
                .expect("seed audit row");

            let pruned_past = storage
                .audit_log()
                .prune_before(OffsetDateTime::now_utc() - TimeDuration::days(365))
                .await
                .expect("prune past");
            assert_eq!(pruned_past, 0);

            let pruned_future = storage
                .audit_log()
                .prune_before(OffsetDateTime::now_utc() + TimeDuration::days(1))
                .await
                .expect("prune future");
            assert_eq!(pruned_future, 1);

            let remaining = storage
                .audit_log()
                .list(AuditLogFilter::default(), None, 10)
                .await
                .expect("list");
            assert!(remaining.items.is_empty());
        });
    }

    #[test]
    fn sqlite_url_detection() {
        assert!(is_sqlite_url("sqlite::memory:"));
        assert!(is_sqlite_url("sqlite:./dev.db"));
        assert!(!is_sqlite_url("postgres://localhost/thewiki"));
    }
}
