//! Migration test: pre-existing `Talk_*` rows get promoted to real talk
//! namespaces (#43, coderabbit).
//!
//! Applies every migration *before* `20260523140000_talk_namespaces.sql`,
//! seeds a rogue `Talk_Foo` row (`is_talk = 0`, NULL pair), then applies
//! the talk migration by hand and asserts the row flipped to `is_talk =
//! 1` and the bidirectional pairing landed.

#![cfg(feature = "sqlite")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use sqlx::Row;
use sqlx::sqlite::SqlitePoolOptions;

/// Migrations that must run before the talk-namespaces backfill. Order
/// matters (sqlx executes them as listed).
const PRE_TALK_MIGRATIONS: &[&str] = &[
    include_str!("../../../migrations/00000000000000_init.sql"),
    include_str!("../../../migrations/20260522005020_sessions.sql"),
    include_str!("../../../migrations/20260522093000_audit_log.sql"),
    include_str!("../../../migrations/20260522163629_page_links.sql"),
    include_str!("../../../migrations/20260522170000_media.sql"),
    include_str!("../../../migrations/20260523000000_media_variants.sql"),
    include_str!("../../../migrations/20260523120000_categories_and_tags.sql"),
];

const TALK_MIGRATION: &str =
    include_str!("../../../migrations/20260523140000_talk_namespaces.sql");

#[tokio::test]
async fn talk_migration_backfills_preexisting_talk_rows() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");

    // Force FK enforcement so the migration's self-referential FK acts the
    // same way the runtime pool does (sqlx-sqlite leaves it off by default).
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .expect("enable fks");

    // Apply every migration strictly before the talk one.
    for sql in PRE_TALK_MIGRATIONS {
        sqlx::raw_sql(sql)
            .execute(&pool)
            .await
            .expect("pre-talk migration");
    }

    // Seed a subject namespace `Foo` and a rogue `Talk_Foo` row that was
    // (hypothetically) inserted by an operator before this migration
    // shipped. Both carry `is_talk` columns absent at this point in the
    // history — the schema only has slug/display_name/created_at.
    let foo_id: [u8; 16] = [1; 16];
    let talk_foo_id: [u8; 16] = [2; 16];
    let unrelated_id: [u8; 16] = [3; 16];
    let ts = "2026-01-01T00:00:00Z";

    sqlx::query("INSERT INTO namespaces (id, slug, display_name, created_at) VALUES (?1, ?2, ?3, ?4)")
        .bind(foo_id.to_vec())
        .bind("Foo")
        .bind("Foo")
        .bind(ts)
        .execute(&pool)
        .await
        .expect("seed subject Foo");
    sqlx::query("INSERT INTO namespaces (id, slug, display_name, created_at) VALUES (?1, ?2, ?3, ?4)")
        .bind(talk_foo_id.to_vec())
        .bind("Talk_Foo")
        .bind("operator-made Talk_Foo")
        .bind(ts)
        .execute(&pool)
        .await
        .expect("seed rogue Talk_Foo");
    // An unrelated subject that the migration should pair up as usual.
    sqlx::query("INSERT INTO namespaces (id, slug, display_name, created_at) VALUES (?1, ?2, ?3, ?4)")
        .bind(unrelated_id.to_vec())
        .bind("Bar")
        .bind("Bar")
        .bind(ts)
        .execute(&pool)
        .await
        .expect("seed unrelated Bar");

    // Apply the talk migration.
    sqlx::raw_sql(TALK_MIGRATION)
        .execute(&pool)
        .await
        .expect("apply talk migration");

    // 1. Rogue `Talk_Foo` got promoted in place.
    let row = sqlx::query("SELECT is_talk, paired_namespace_id FROM namespaces WHERE slug = ?1")
        .bind("Talk_Foo")
        .fetch_one(&pool)
        .await
        .expect("fetch Talk_Foo");
    let is_talk: bool = row.get(0);
    let paired: Vec<u8> = row.get(1);
    assert!(is_talk, "rogue Talk_Foo should be promoted to is_talk = true");
    assert_eq!(paired, foo_id.to_vec(), "Talk_Foo must point back at Foo");

    // 2. Subject `Foo` got the back-pointer at the same `Talk_Foo` row
    //    (no duplicate Talk_Foo created).
    let row = sqlx::query("SELECT id, paired_namespace_id FROM namespaces WHERE slug = ?1")
        .bind("Foo")
        .fetch_one(&pool)
        .await
        .expect("fetch Foo");
    let foo_row_id: Vec<u8> = row.get(0);
    let foo_paired: Vec<u8> = row.get(1);
    assert_eq!(foo_row_id, foo_id.to_vec());
    assert_eq!(
        foo_paired,
        talk_foo_id.to_vec(),
        "Foo must pair at the same rogue Talk_Foo row, not a freshly minted one",
    );

    // 3. Only one row named `Talk_Foo` exists — no shadow row leaked in.
    let row = sqlx::query("SELECT COUNT(*) as c FROM namespaces WHERE slug = ?1")
        .bind("Talk_Foo")
        .fetch_one(&pool)
        .await
        .expect("count Talk_Foo");
    let c: i64 = row.get("c");
    assert_eq!(c, 1, "exactly one Talk_Foo row should exist post-migration");

    // 4. The unrelated `Bar` subject got a freshly created `Talk_Bar`
    //    partner with the expected display name.
    let row = sqlx::query(
        "SELECT is_talk, paired_namespace_id, display_name FROM namespaces WHERE slug = ?1",
    )
    .bind("Talk_Bar")
    .fetch_one(&pool)
    .await
    .expect("fetch Talk_Bar");
    let bar_talk_is_talk: bool = row.get(0);
    let bar_paired: Vec<u8> = row.get(1);
    let bar_display: String = row.get(2);
    assert!(bar_talk_is_talk);
    assert_eq!(bar_paired, unrelated_id.to_vec());
    assert_eq!(bar_display, "Talk: Bar");
}
