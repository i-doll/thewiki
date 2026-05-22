//! Integration coverage for [`PostgresRevisionRepository`].
//!
//! Skipped when no Postgres URL is configured.

#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common_pg;

use common_pg::{fresh_storage, make_namespace, make_page, make_user};
use thewiki_core::Revision;
use thewiki_storage::StorageError;
use thewiki_storage::repo::{
    NamespaceRepository, PageRepository, RevisionRepository, UserRepository,
};

#[tokio::test]
async fn create_and_get_by_id_round_trips() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };

    let ns = make_namespace("main");
    let user = make_user("alice");
    let page = make_page(ns.id, "home");
    storage.namespaces().create(&ns).await.expect("ns");
    storage.users().create(&user, None).await.expect("user");
    storage.pages().create(&page).await.expect("page");

    let rev = Revision::new(
        page.id,
        None,
        user.id,
        "# Hello".to_string(),
        Some("initial".to_string()),
    );
    storage.revisions().create(&rev).await.expect("create rev");

    let loaded = storage
        .revisions()
        .get_by_id(rev.id)
        .await
        .expect("get rev");
    assert_eq!(loaded.id, rev.id);
    assert_eq!(loaded.page_id, page.id);
    assert_eq!(loaded.author_id, user.id);
    assert_eq!(loaded.body, "# Hello");
    assert_eq!(loaded.edit_summary.as_deref(), Some("initial"));
    assert!(loaded.is_initial());
}

#[tokio::test]
async fn head_of_returns_newest_revision() {
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
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let r3 = Revision::new(page.id, Some(r2.id), user.id, "v3".into(), None);
    storage.revisions().create(&r3).await.expect("r3");

    let head = storage.revisions().head_of(page.id).await.expect("head");
    assert_eq!(head.id, r3.id);
    assert_eq!(head.parent_id, Some(r2.id));
}

#[tokio::test]
async fn head_of_with_no_revisions_is_not_found() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    let page = make_page(ns.id, "home");
    storage.namespaces().create(&ns).await.expect("ns");
    storage.pages().create(&page).await.expect("page");

    let err = storage
        .revisions()
        .head_of(page.id)
        .await
        .expect_err("no revs");
    assert!(matches!(err, StorageError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn list_for_page_orders_newest_first_and_paginates() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };

    let ns = make_namespace("main");
    let user = make_user("alice");
    let page = make_page(ns.id, "home");
    storage.namespaces().create(&ns).await.expect("ns");
    storage.users().create(&user, None).await.expect("user");
    storage.pages().create(&page).await.expect("page");

    let mut revs = Vec::new();
    let mut parent = None;
    for n in 0..5u8 {
        let r = Revision::new(page.id, parent, user.id, format!("v{n}"), None);
        storage.revisions().create(&r).await.expect("rev");
        parent = Some(r.id);
        revs.push(r);
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    let first = storage
        .revisions()
        .list_for_page(page.id, None, 2)
        .await
        .expect("page 1");
    assert_eq!(first.items.len(), 2);
    assert!(first.next.is_some());
    assert_eq!(first.items[0].id, revs[4].id);
    assert_eq!(first.items[1].id, revs[3].id);

    let second = storage
        .revisions()
        .list_for_page(page.id, first.next.clone(), 2)
        .await
        .expect("page 2");
    assert_eq!(second.items.len(), 2);
    assert!(second.next.is_some());
    assert_eq!(second.items[0].id, revs[2].id);
    assert_eq!(second.items[1].id, revs[1].id);

    let third = storage
        .revisions()
        .list_for_page(page.id, second.next.clone(), 2)
        .await
        .expect("page 3");
    assert_eq!(third.items.len(), 1);
    assert!(third.next.is_none(), "out of rows");
    assert_eq!(third.items[0].id, revs[0].id);
}
