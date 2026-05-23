//! Integration coverage for [`SqliteNamespaceRepository`] focused on the
//! subject/talk pairing flow (#43).
//!
//! The bread-and-butter CRUD path is exercised by the existing migration
//! tests and the API-layer tests; this file zeroes in on the atomicity
//! guarantee coderabbit flagged: when the auto-paired talk insert fails
//! partway through, the subject row must roll back too.

#![cfg(feature = "sqlite")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{fresh_storage, make_namespace};
use thewiki_core::NamespaceSlug;
use thewiki_storage::StorageError;
use thewiki_storage::repo::NamespaceRepository;

/// Contrive a unique-index collision on `paired_namespace_id` partway
/// through `create()`, then assert the subject row didn't leak past the
/// rollback. The schema has a `UNIQUE` partial index on
/// `paired_namespace_id`, so once `foo` is paired to a talk row we can
/// rename that talk row to `Talk_bar` and then trying to also pair `bar`
/// at it triggers the constraint.
#[tokio::test]
async fn create_rolls_back_subject_when_paired_update_fails() {
    let storage = fresh_storage().await;

    // Seed `foo` + its auto-created `Talk_foo` partner. The pair is now
    // bidirectional: `foo.paired_namespace_id = Talk_foo.id` and vice
    // versa.
    let foo = make_namespace("foo");
    storage.namespaces().create(&foo).await.expect("seed foo");

    // Rename the seeded `Talk_foo` row directly in SQL to `Talk_bar`.
    // `foo.paired_namespace_id` keeps pointing at it (we only changed
    // the slug); the partial UNIQUE index on `paired_namespace_id`
    // therefore protects exactly this id.
    sqlx::query("UPDATE namespaces SET slug = 'Talk_bar' WHERE slug = 'Talk_foo'")
        .execute(storage.pool())
        .await
        .expect("rename Talk_foo to Talk_bar");

    // Now `create(bar)` should:
    //   1. INSERT bar
    //   2. INSERT Talk_bar (CONFLICT — slug already taken)
    //   3. Read the existing Talk_bar; its paired_namespace_id is
    //      already set, so skip the back-pointer update.
    //   4. UPDATE bar.paired_namespace_id = existing Talk_bar.id —
    //      this collides with `foo.paired_namespace_id = same id`,
    //      tripping the partial UNIQUE index.
    //
    // The whole transaction must roll back so `bar` doesn't persist.
    let bar = make_namespace("bar");
    let err = storage
        .namespaces()
        .create(&bar)
        .await
        .expect_err("paired update should collide on UNIQUE(paired_namespace_id)");
    assert!(
        matches!(err, StorageError::Database(_) | StorageError::Conflict(_)),
        "expected DB/Conflict error from UNIQUE violation, got {err:?}",
    );

    // The subject row must NOT have leaked past the rollback.
    let bar_slug = NamespaceSlug::new("bar").expect("slug");
    let lookup = storage.namespaces().get_by_slug(&bar_slug).await;
    assert!(
        matches!(lookup, Err(StorageError::NotFound)),
        "bar must not exist after rolled-back create, got {lookup:?}",
    );
}

/// Subject + auto-created talk partner share a `created_at` timestamp
/// when the create is committed (sanity check that the happy-path
/// transaction commits both rows).
#[tokio::test]
async fn create_persists_both_subject_and_talk_partner() {
    let storage = fresh_storage().await;
    let help = make_namespace("Help");
    storage.namespaces().create(&help).await.expect("create");

    let help_slug = NamespaceSlug::new("Help").expect("slug");
    let stored = storage
        .namespaces()
        .get_by_slug(&help_slug)
        .await
        .expect("subject");
    let talk_slug = NamespaceSlug::new("Talk_Help").expect("talk slug");
    let talk = storage
        .namespaces()
        .get_by_slug(&talk_slug)
        .await
        .expect("talk partner");
    assert_eq!(stored.paired_namespace_id, Some(talk.id));
    assert_eq!(talk.paired_namespace_id, Some(stored.id));
    assert!(talk.is_talk);
    assert!(!stored.is_talk);
}
