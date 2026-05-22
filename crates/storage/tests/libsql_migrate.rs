//! Integration coverage for the libsql migration runner.
//!
//! Boots a fresh in-memory libsql instance, applies the full embedded
//! migration set, and verifies the schema landed.

#![cfg(feature = "libsql")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common_libsql;

use common_libsql::fresh_storage;

/// Tables every migration is required to create.
const EXPECTED_TABLES: &[&str] = &[
    "namespaces",
    "users",
    "roles",
    "user_roles",
    "pages",
    "revisions",
    "sessions",
    "audit_log",
];

#[tokio::test]
async fn migrations_create_every_expected_table() {
    let storage = fresh_storage().await;
    let conn = storage.connection();
    for table in EXPECTED_TABLES {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [(*table).to_owned()],
            )
            .await
            .expect("query sqlite_master");
        let row = rows
            .next()
            .await
            .expect("rows iterator")
            .expect("expected one count row");
        let count: i64 = row.get(0).expect("count column");
        assert_eq!(count, 1, "table {table} missing after migrations");
    }
}

#[tokio::test]
async fn migrations_are_idempotent_when_rerun() {
    // Running the migrator twice over the same connection must be a no-op the
    // second time — the `_libsql_migrations` row gates re-application.
    let storage = fresh_storage().await;
    thewiki_storage::libsql::LibsqlStorage::migrate(storage.connection())
        .await
        .expect("rerun migrate");

    // Pages table still exists and is empty.
    let mut rows = storage
        .connection()
        .query("SELECT COUNT(*) FROM pages", ())
        .await
        .expect("count pages");
    let row = rows
        .next()
        .await
        .expect("rows iterator")
        .expect("expected one count row");
    let count: i64 = row.get(0).expect("count column");
    assert_eq!(count, 0);
}
