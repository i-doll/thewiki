//! Integration coverage for [`PostgresPageRepository`].
//!
//! Skipped when no Postgres URL is configured; otherwise mirrors the SQLite
//! page coverage so the two backends stay in lockstep.

#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common_pg;

use common_pg::{fresh_storage, make_namespace, make_page};
use thewiki_core::ProtectionLevel;
use thewiki_storage::StorageError;
use thewiki_storage::repo::{NamespaceRepository, PageRepository};

#[tokio::test]
async fn create_then_get_by_id_round_trips() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");

    let page = make_page(ns.id, "home");
    storage.pages().create(&page).await.expect("create page");

    let loaded = storage.pages().get_by_id(page.id).await.expect("get");
    assert_eq!(loaded.id, page.id);
    assert_eq!(loaded.namespace_id, ns.id);
    assert_eq!(loaded.slug, "home");
    assert_eq!(loaded.title, "home");
    assert_eq!(loaded.content_format, page.content_format);
    assert_eq!(loaded.protection_level, page.protection_level);
}

#[tokio::test]
async fn get_by_namespace_and_slug_resolves() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");

    let page = make_page(ns.id, "welcome");
    storage.pages().create(&page).await.expect("create page");

    let loaded = storage
        .pages()
        .get_by_namespace_and_slug(ns.id, "welcome")
        .await
        .expect("resolve");
    assert_eq!(loaded.id, page.id);
}

#[tokio::test]
async fn missing_page_is_not_found() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");

    let err = storage
        .pages()
        .get_by_namespace_and_slug(ns.id, "absent")
        .await
        .expect_err("expect not-found");
    assert!(matches!(err, StorageError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn duplicate_slug_in_namespace_conflicts() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");

    let p1 = make_page(ns.id, "home");
    storage.pages().create(&p1).await.expect("first ok");

    let p2 = make_page(ns.id, "home");
    let err = storage.pages().create(&p2).await.expect_err("dup");
    assert!(matches!(err, StorageError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn same_slug_different_namespaces_is_allowed() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let main = make_namespace("main");
    let help = make_namespace("help");
    storage.namespaces().create(&main).await.expect("ns 1");
    storage.namespaces().create(&help).await.expect("ns 2");

    let p1 = make_page(main.id, "home");
    let p2 = make_page(help.id, "home");
    storage.pages().create(&p1).await.expect("main/home");
    storage.pages().create(&p2).await.expect("help/home");
}

#[tokio::test]
async fn update_persists_mutable_fields() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");
    let mut page = make_page(ns.id, "home");
    storage.pages().create(&page).await.expect("create");

    page.title = "Home (renamed)".into();
    page.protection_level = ProtectionLevel::SemiProtected;
    page.updated_at = time::OffsetDateTime::now_utc();
    storage.pages().update(&page).await.expect("update");

    let loaded = storage.pages().get_by_id(page.id).await.expect("get");
    assert_eq!(loaded.title, "Home (renamed)");
    assert_eq!(loaded.protection_level, ProtectionLevel::SemiProtected);
}

#[tokio::test]
async fn update_missing_page_is_not_found() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");
    let page = make_page(ns.id, "ghost");

    let err = storage.pages().update(&page).await.expect_err("not found");
    assert!(matches!(err, StorageError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn delete_removes_row() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");
    let page = make_page(ns.id, "home");
    storage.pages().create(&page).await.expect("create");

    storage.pages().delete(page.id).await.expect("delete");
    let err = storage.pages().get_by_id(page.id).await.expect_err("gone");
    assert!(matches!(err, StorageError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn delete_missing_page_is_not_found() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");
    let page = make_page(ns.id, "ghost");

    let err = storage
        .pages()
        .delete(page.id)
        .await
        .expect_err("not found");
    assert!(matches!(err, StorageError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn list_in_namespace_pages_across_cursors() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");

    let mut ids = Vec::new();
    for n in 0..5u8 {
        let p = make_page(ns.id, &format!("page-{n}"));
        storage.pages().create(&p).await.expect("create");
        ids.push(p.id);
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    let first = storage
        .pages()
        .list_in_namespace(ns.id, None, 2)
        .await
        .expect("page 1");
    assert_eq!(first.items.len(), 2);
    assert!(first.next.is_some());
    assert_eq!(first.items[0].id, ids[0]);
    assert_eq!(first.items[1].id, ids[1]);

    let second = storage
        .pages()
        .list_in_namespace(ns.id, first.next.clone(), 2)
        .await
        .expect("page 2");
    assert_eq!(second.items.len(), 2);
    assert!(second.next.is_some());
    assert_eq!(second.items[0].id, ids[2]);
    assert_eq!(second.items[1].id, ids[3]);

    let third = storage
        .pages()
        .list_in_namespace(ns.id, second.next.clone(), 2)
        .await
        .expect("page 3");
    assert_eq!(third.items.len(), 1);
    assert!(third.next.is_none(), "no more rows");
    assert_eq!(third.items[0].id, ids[4]);
}

#[tokio::test]
async fn list_in_namespace_clamps_zero_limit_to_default() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");

    let p = make_page(ns.id, "only");
    storage.pages().create(&p).await.expect("create");

    let listed = storage
        .pages()
        .list_in_namespace(ns.id, None, 0)
        .await
        .expect("list");
    assert_eq!(listed.items.len(), 1);
    assert!(listed.next.is_none());
}
