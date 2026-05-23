//! Integration coverage for [`LibsqlNamespaceRepository`] focused on the
//! subject/talk pairing flow (#43).
//!
//! Mirrors `tests/sqlite_namespaces.rs` — the focus is on the atomicity
//! guarantee coderabbit flagged: when the auto-paired talk insert fails
//! partway through, the subject row must roll back too.

#![cfg(feature = "libsql")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common_libsql;

use common_libsql::{fresh_storage, make_namespace};
use libsql::{Value, params};
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

    let foo = make_namespace("foo");
    storage.namespaces().create(&foo).await.expect("seed foo");

    // Rename the seeded `Talk_foo` row directly so `foo` keeps pointing
    // at the same id but under a new slug. That id is now the
    // forbidden duplicate for the partial UNIQUE index when we later
    // try to make `bar` point at it too.
    storage
        .connection()
        .execute(
            "UPDATE namespaces SET slug = ?1 WHERE slug = ?2",
            params![
                Value::Text("Talk_bar".to_string()),
                Value::Text("Talk_foo".to_string()),
            ],
        )
        .await
        .expect("rename Talk_foo to Talk_bar");

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

    let bar_slug = NamespaceSlug::new("bar").expect("slug");
    let lookup = storage.namespaces().get_by_slug(&bar_slug).await;
    assert!(
        matches!(lookup, Err(StorageError::NotFound)),
        "bar must not exist after rolled-back create, got {lookup:?}",
    );
}

/// Sanity check: happy-path `create()` commits both the subject and its
/// auto-paired talk partner.
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
