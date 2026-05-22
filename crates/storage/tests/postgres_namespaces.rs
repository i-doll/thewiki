//! Integration coverage for [`PostgresNamespaceRepository`].

#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common_pg;

use common_pg::{fresh_storage, make_namespace};
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
