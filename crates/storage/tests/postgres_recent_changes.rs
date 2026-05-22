//! Integration coverage for [`PostgresRecentChangesRepository`].

#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common_pg;

use common_pg::{fresh_storage, make_namespace, make_page, make_user};
use thewiki_core::Revision;
use thewiki_storage::StorageError;
use thewiki_storage::repo::{
    Cursor, NamespaceRepository, PageRepository, RecentChangesFilter, RecentChangesRepository,
    RevisionRepository, UserRepository,
};

#[tokio::test]
async fn list_returns_revisions_newest_first() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    let user = make_user("alice");
    let page = make_page(ns.id, "home");
    storage.namespaces().create(&ns).await.expect("ns");
    storage.users().create(&user, None).await.expect("user");
    storage.pages().create(&page).await.expect("page");

    let r1 = Revision::new(page.id, None, user.id, "v1".into(), None);
    storage.revisions().create(&r1).await.expect("r1");
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let r2 = Revision::new(page.id, Some(r1.id), user.id, "v2".into(), None);
    storage.revisions().create(&r2).await.expect("r2");

    let listed = storage
        .recent_changes()
        .list(RecentChangesFilter::default(), None, 10)
        .await
        .expect("list");
    assert_eq!(listed.items.len(), 2);
    assert_eq!(listed.items[0].revision_id, r2.id);
    assert_eq!(listed.items[1].revision_id, r1.id);
    assert_eq!(listed.items[0].page_slug, "home");
    assert_eq!(listed.items[0].namespace_slug, "main");
    assert_eq!(listed.items[0].author_username, "alice");
}

#[tokio::test]
async fn list_filters_by_namespace_and_actor() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let main = make_namespace("main");
    let help = make_namespace("help");
    storage.namespaces().create(&main).await.expect("main");
    storage.namespaces().create(&help).await.expect("help");
    let alice = make_user("alice");
    let bob = make_user("bob");
    storage.users().create(&alice, None).await.expect("alice");
    storage.users().create(&bob, None).await.expect("bob");
    let main_page = make_page(main.id, "home");
    let help_page = make_page(help.id, "intro");
    storage.pages().create(&main_page).await.expect("main pg");
    storage.pages().create(&help_page).await.expect("help pg");

    storage
        .revisions()
        .create(&Revision::new(
            main_page.id,
            None,
            alice.id,
            "a".into(),
            None,
        ))
        .await
        .expect("alice main");
    storage
        .revisions()
        .create(&Revision::new(help_page.id, None, bob.id, "b".into(), None))
        .await
        .expect("bob help");

    let main_only = storage
        .recent_changes()
        .list(
            RecentChangesFilter {
                namespace_id: Some(main.id),
                ..Default::default()
            },
            None,
            10,
        )
        .await
        .expect("ns filter");
    assert_eq!(main_only.items.len(), 1);
    assert_eq!(main_only.items[0].namespace_slug, "main");

    let bob_only = storage
        .recent_changes()
        .list(
            RecentChangesFilter {
                actor_id: Some(bob.id),
                ..Default::default()
            },
            None,
            10,
        )
        .await
        .expect("actor filter");
    assert_eq!(bob_only.items.len(), 1);
    assert_eq!(bob_only.items[0].author_username, "bob");
}

#[tokio::test]
async fn malformed_cursor_is_invalid_input() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let err = storage
        .recent_changes()
        .list(
            RecentChangesFilter::default(),
            Some(Cursor("garbage".to_string())),
            10,
        )
        .await
        .expect_err("invalid cursor");
    assert!(matches!(err, StorageError::InvalidInput(_)), "got {err:?}");
}
