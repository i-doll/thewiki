//! Bootstrap behaviour for [`SqliteStorage::new`].
//!
//! Documents the contract that `docker run -v thewiki-data:/data ...` relies
//! on: pointed at a path whose file *and* parent directory don't yet exist,
//! the constructor must (a) materialise the directory, (b) create the SQLite
//! file via sqlx's `create_if_missing`, and (c) apply the embedded migration
//! set so the resulting database is immediately usable.

#![cfg(feature = "sqlite")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use sqlx::Row;
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};

fn default_opts() -> SqliteOptions {
    SqliteOptions {
        max_connections: 1,
        acquire_timeout: Duration::from_secs(5),
        foreign_keys: true,
    }
}

#[tokio::test]
async fn creates_database_file_and_parent_directory() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    // Two levels deep, neither of which exists yet — this is the
    // `/data/nested/db.sqlite` shape the Docker image needs to support.
    let nested = tmp.path().join("nested").join("more");
    let db_path = nested.join("thewiki.db");
    assert!(
        !nested.exists(),
        "precondition: nested parent must not exist yet"
    );

    let url = format!("sqlite://{}", db_path.display());
    let storage = SqliteStorage::new(&url, default_opts())
        .await
        .expect("open storage at a fresh nested path");

    assert!(db_path.exists(), "sqlite file should have been created");
    assert!(
        nested.is_dir(),
        "parent directory should have been created"
    );

    // Confirm at least one migration ran by looking for a known table.
    let row = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'pages'")
        .fetch_optional(storage.pool())
        .await
        .expect("query sqlite_master");
    let row = row.expect("`pages` table must exist after migrations");
    let name: String = row.try_get("name").expect("read name column");
    assert_eq!(name, "pages");
}

#[tokio::test]
async fn in_memory_url_still_works() {
    // Regression guard: the parent-directory creation must not regress the
    // in-memory boot used by every other integration test in this crate.
    let storage = SqliteStorage::new("sqlite::memory:", default_opts())
        .await
        .expect("open in-memory sqlite");
    let row = sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'pages'")
        .fetch_optional(storage.pool())
        .await
        .expect("query sqlite_master");
    assert!(row.is_some(), "migrations run against in-memory URL too");
}
