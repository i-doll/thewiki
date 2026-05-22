//! Integration coverage for the Postgres migration set.
//!
//! Skipped when no Postgres URL is configured.

#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common_pg;

use common_pg::fresh_pool;
use sqlx::Row;

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
async fn initial_migration_creates_all_tables() {
    let Some(fresh) = fresh_pool().await else {
        return;
    };

    for table in EXPECTED_TABLES {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_name = $1",
        )
        .bind(table)
        .fetch_one(&fresh.pool)
        .await
        .expect("query information_schema");
        assert_eq!(row.0, 1, "table {table} missing after migrations");
    }
}

#[tokio::test]
async fn current_revision_fk_is_deferrable() {
    let Some(fresh) = fresh_pool().await else {
        return;
    };

    let row = sqlx::query(
        "SELECT con.condeferrable AS deferrable, con.condeferred AS deferred \
         FROM pg_constraint con \
         JOIN pg_class cls ON cls.oid = con.conrelid \
         WHERE cls.relname = 'pages' \
           AND con.conname = 'pages_current_revision_id_fkey'",
    )
    .fetch_one(&fresh.pool)
    .await
    .expect("constraint metadata");
    let deferrable: bool = row.get("deferrable");
    let deferred: bool = row.get("deferred");
    assert!(deferrable, "FK must be DEFERRABLE");
    assert!(deferred, "FK must be INITIALLY DEFERRED");
}

#[tokio::test]
async fn deferred_fk_lets_a_transaction_seed_page_and_revision_in_order() {
    use thewiki_core::{ContentFormat, NamespaceId, PageId, ProtectionLevel, RevisionId, UserId};
    use time::OffsetDateTime;
    use uuid::Uuid;

    let Some(fresh) = fresh_pool().await else {
        return;
    };
    let pool = fresh.pool.clone();

    // Seed a namespace + user so the FKs from `pages.namespace_id` and
    // `revisions.author_id` resolve.
    let ns_id: Uuid = NamespaceId::new().into_uuid();
    let user_id: Uuid = UserId::new().into_uuid();
    let now = OffsetDateTime::now_utc();
    sqlx::query(
        "INSERT INTO namespaces (id, slug, display_name, created_at) VALUES ($1, 'main', 'Main', $2)",
    )
    .bind(ns_id)
    .bind(now)
    .execute(&pool)
    .await
    .expect("seed namespace");
    sqlx::query("INSERT INTO users (id, username, created_at) VALUES ($1, 'alice', $2)")
        .bind(user_id)
        .bind(now)
        .execute(&pool)
        .await
        .expect("seed user");

    // Now do the cyclic insert: page references revision that doesn't exist
    // yet. The deferred FK means the check waits until COMMIT, by which time
    // we've appended the revision.
    let page_id: Uuid = PageId::new().into_uuid();
    let rev_id: Uuid = RevisionId::new().into_uuid();
    let mut tx = pool.begin().await.expect("begin");
    sqlx::query(
        "INSERT INTO pages
            (id, namespace_id, slug, title, current_revision_id, content_format, protection_level, created_at, updated_at)
         VALUES ($1, $2, 'home', 'Home', $3, $4, $5, $6, $6)",
    )
    .bind(page_id)
    .bind(ns_id)
    .bind(rev_id)
    .bind(ContentFormat::Markdown.as_str())
    .bind(ProtectionLevel::None.as_str())
    .bind(now)
    .execute(&mut *tx)
    .await
    .expect("insert page with forward FK");

    sqlx::query(
        "INSERT INTO revisions
            (id, page_id, parent_id, author_id, body, edit_summary, created_at)
         VALUES ($1, $2, NULL, $3, 'body', NULL, $4)",
    )
    .bind(rev_id)
    .bind(page_id)
    .bind(user_id)
    .bind(now)
    .execute(&mut *tx)
    .await
    .expect("insert revision after page");

    tx.commit().await.expect("commit with FK satisfied");
}
