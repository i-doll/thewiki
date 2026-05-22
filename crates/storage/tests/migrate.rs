//! Integration test for the initial migration.
//!
//! Boots an in-memory SQLite, runs every migration under `/migrations`, then
//! probes the resulting schema to make sure the tables and the key uniqueness
//! constraints landed.

#![cfg(feature = "sqlite")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use sqlx::Row;
use sqlx::sqlite::SqlitePoolOptions;

/// Tables the inaugural migration is required to create.
const EXPECTED_TABLES: &[&str] = &[
    "namespaces",
    "users",
    "roles",
    "user_roles",
    "pages",
    "revisions",
];

#[tokio::test]
async fn initial_migration_creates_all_tables() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("run migrations");

    for table in EXPECTED_TABLES {
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1")
                .bind(table)
                .fetch_one(&pool)
                .await
                .expect("query sqlite_master");
        assert_eq!(row.0, 1, "table {table} missing after migrations");
    }
}

#[tokio::test]
async fn users_username_is_unique() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("run migrations");

    // Two distinct 16-byte BLOB IDs, same username — second insert must fail.
    let id_a: [u8; 16] = [1; 16];
    let id_b: [u8; 16] = [2; 16];
    let created_at = "2026-01-01T00:00:00Z";

    sqlx::query("INSERT INTO users (id, username, created_at) VALUES (?1, ?2, ?3)")
        .bind(&id_a[..])
        .bind("alice")
        .bind(created_at)
        .execute(&pool)
        .await
        .expect("first insert succeeds");

    let dup = sqlx::query("INSERT INTO users (id, username, created_at) VALUES (?1, ?2, ?3)")
        .bind(&id_b[..])
        .bind("alice")
        .bind(created_at)
        .execute(&pool)
        .await;
    assert!(
        dup.is_err(),
        "duplicate username should violate UNIQUE constraint",
    );
}

#[tokio::test]
async fn pages_namespace_slug_pair_is_unique() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("run migrations");

    // sqlx's sqlite driver enables foreign keys by default, so seed the
    // referenced namespace first.
    let ns_id: [u8; 16] = [9; 16];
    let page_a: [u8; 16] = [0xa; 16];
    let page_b: [u8; 16] = [0xb; 16];
    let ts = "2026-01-01T00:00:00Z";

    sqlx::query(
        "INSERT INTO namespaces (id, slug, display_name, created_at) VALUES (?1, ?2, ?3, ?4)",
    )
    .bind(ns_id.to_vec())
    .bind("main")
    .bind("Main")
    .bind(ts)
    .execute(&pool)
    .await
    .expect("seed namespace");

    let insert_page = |id: &[u8]| {
        sqlx::query(
            "INSERT INTO pages (id, namespace_id, slug, title, content_format, protection_level, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )
        .bind(id.to_vec())
        .bind(ns_id.to_vec())
        .bind("home")
        .bind("Home")
        .bind("markdown")
        .bind("public")
        .bind(ts)
        .bind(ts)
        .execute(&pool)
    };

    insert_page(&page_a).await.expect("first page");
    let dup = insert_page(&page_b).await;
    assert!(
        dup.is_err(),
        "duplicate (namespace_id, slug) should violate UNIQUE constraint",
    );
}

#[tokio::test]
async fn history_index_is_present() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("run migrations");

    let row = sqlx::query(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND name = 'idx_revisions_page_id_created_at'",
    )
    .fetch_optional(&pool)
    .await
    .expect("query sqlite_master");
    let name: String = row.expect("index missing").get("name");
    assert_eq!(name, "idx_revisions_page_id_created_at");
}
