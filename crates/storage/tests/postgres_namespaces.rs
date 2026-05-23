//! Integration coverage for [`PostgresNamespaceRepository`].

#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common_pg;

use common_pg::{fresh_storage, make_namespace};
use sqlx::Executor;
use thewiki_core::NamespaceSlug;
use thewiki_storage::StorageError;
use thewiki_storage::repo::NamespaceRepository;

#[tokio::test]
async fn create_then_get_round_trips() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("create");

    let by_id = storage.namespaces().get_by_id(ns.id).await.expect("by id");
    assert_eq!(by_id.id, ns.id);
    assert_eq!(by_id.slug, ns.slug);
    assert_eq!(by_id.display_name, ns.display_name);

    let slug = NamespaceSlug::new("main").expect("slug");
    let by_slug = storage
        .namespaces()
        .get_by_slug(&slug)
        .await
        .expect("by slug");
    assert_eq!(by_slug.id, ns.id);
}

#[tokio::test]
async fn duplicate_namespace_slug_conflicts() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let a = make_namespace("main");
    storage.namespaces().create(&a).await.expect("first");
    let b = make_namespace("main");
    let err = storage.namespaces().create(&b).await.expect_err("dup");
    assert!(matches!(err, StorageError::Conflict(_)), "got {err:?}");
}

/// Contrive a unique-index collision on `paired_namespace_id` partway
/// through `create()` (#43, coderabbit). The subject row must NOT leak
/// past the rollback when the back-pointer update fails.
#[tokio::test]
async fn create_rolls_back_subject_when_paired_update_fails() {
    let Some((storage, fresh)) = fresh_storage().await else {
        return;
    };

    // Seed `foo`; this auto-creates `Talk_foo` paired bidirectionally.
    let foo = make_namespace("foo");
    storage.namespaces().create(&foo).await.expect("seed foo");

    // Rename the seeded `Talk_foo` to `Talk_bar` directly in SQL. The
    // pair stays intact — `foo.paired_namespace_id` keeps pointing at
    // the same id under a new slug. That id is now the forbidden
    // duplicate for the partial UNIQUE index when we later try to make
    // `bar` point at it too.
    fresh.pool
        .execute("UPDATE namespaces SET slug = 'Talk_bar' WHERE slug = 'Talk_foo'")
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

    // The subject must NOT have leaked past the rollback.
    let bar_slug = NamespaceSlug::new("bar").expect("slug");
    let lookup = storage.namespaces().get_by_slug(&bar_slug).await;
    assert!(
        matches!(lookup, Err(StorageError::NotFound)),
        "bar must not exist after rolled-back create, got {lookup:?}",
    );
}

#[tokio::test]
async fn list_returns_every_namespace() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    storage
        .namespaces()
        .create(&make_namespace("main"))
        .await
        .expect("main");
    storage
        .namespaces()
        .create(&make_namespace("help"))
        .await
        .expect("help");

    let all = storage.namespaces().list().await.expect("list");
    let slugs: Vec<_> = all.iter().map(|n| n.slug.as_str().to_owned()).collect();
    assert_eq!(slugs.len(), 2);
    assert!(slugs.contains(&"main".to_owned()));
    assert!(slugs.contains(&"help".to_owned()));
}
