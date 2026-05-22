//! Integration coverage for [`SqlitePageLinkRepository`].
//!
//! Each test boots a fresh in-memory database, seeds a namespace and a few
//! pages, then exercises the page-link mutation/query surface. The repo is
//! deliberately tiny — `replace_for_source` is the only mutator and the
//! query side is `list_backlinks_to` — so the suite is correspondingly
//! short.

#![cfg(feature = "sqlite")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{fresh_storage, make_namespace, make_page};
use thewiki_storage::repo::{NamespaceRepository, PageLink, PageLinkRepository, PageRepository};

#[tokio::test]
async fn replace_then_list_returns_inserted_rows() {
    let storage = fresh_storage().await;
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");
    let source = make_page(ns.id, "src");
    storage.pages().create(&source).await.expect("seed src");

    let links = vec![PageLink {
        source_page_id: source.id,
        target_namespace_slug: "main".into(),
        target_page_slug: "target".into(),
    }];
    storage
        .page_links()
        .replace_for_source(source.id, &links)
        .await
        .expect("replace");

    let slice = storage
        .page_links()
        .list_backlinks_to("main", "target", None, 50)
        .await
        .expect("list");
    assert_eq!(slice.items.len(), 1);
    assert_eq!(slice.items[0].source_page_id, source.id);
    assert_eq!(slice.items[0].source_namespace_slug, "main");
    assert_eq!(slice.items[0].source_page_slug, "src");
}

#[tokio::test]
async fn replace_for_source_is_an_atomic_swap() {
    let storage = fresh_storage().await;
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");
    let source = make_page(ns.id, "src");
    storage.pages().create(&source).await.expect("seed src");

    storage
        .page_links()
        .replace_for_source(
            source.id,
            &[PageLink {
                source_page_id: source.id,
                target_namespace_slug: "main".into(),
                target_page_slug: "old".into(),
            }],
        )
        .await
        .expect("first replace");

    // Replace with a fresh set — the old row must be gone.
    storage
        .page_links()
        .replace_for_source(
            source.id,
            &[PageLink {
                source_page_id: source.id,
                target_namespace_slug: "main".into(),
                target_page_slug: "new".into(),
            }],
        )
        .await
        .expect("second replace");

    let old = storage
        .page_links()
        .list_backlinks_to("main", "old", None, 50)
        .await
        .expect("list old");
    assert!(old.items.is_empty(), "old target should have no backlinks");

    let fresh = storage
        .page_links()
        .list_backlinks_to("main", "new", None, 50)
        .await
        .expect("list new");
    assert_eq!(fresh.items.len(), 1);
}

#[tokio::test]
async fn duplicate_entries_in_input_dedupe_via_or_ignore() {
    let storage = fresh_storage().await;
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");
    let source = make_page(ns.id, "src");
    storage.pages().create(&source).await.expect("seed src");

    let dup = vec![
        PageLink {
            source_page_id: source.id,
            target_namespace_slug: "main".into(),
            target_page_slug: "target".into(),
        },
        PageLink {
            source_page_id: source.id,
            target_namespace_slug: "main".into(),
            target_page_slug: "target".into(),
        },
    ];
    storage
        .page_links()
        .replace_for_source(source.id, &dup)
        .await
        .expect("replace with dups");

    let slice = storage
        .page_links()
        .list_backlinks_to("main", "target", None, 50)
        .await
        .expect("list");
    assert_eq!(slice.items.len(), 1, "duplicate entries should be merged");
}

#[tokio::test]
async fn delete_for_source_clears_all_outbound_rows() {
    let storage = fresh_storage().await;
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");
    let source = make_page(ns.id, "src");
    storage.pages().create(&source).await.expect("seed src");

    storage
        .page_links()
        .replace_for_source(
            source.id,
            &[PageLink {
                source_page_id: source.id,
                target_namespace_slug: "main".into(),
                target_page_slug: "target".into(),
            }],
        )
        .await
        .expect("seed link");

    storage
        .page_links()
        .delete_for_source(source.id)
        .await
        .expect("delete");

    let slice = storage
        .page_links()
        .list_backlinks_to("main", "target", None, 50)
        .await
        .expect("list");
    assert!(slice.items.is_empty());
}

#[tokio::test]
async fn page_delete_cascades_to_page_links() {
    let storage = fresh_storage().await;
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");
    let source = make_page(ns.id, "src");
    storage.pages().create(&source).await.expect("seed src");

    storage
        .page_links()
        .replace_for_source(
            source.id,
            &[PageLink {
                source_page_id: source.id,
                target_namespace_slug: "main".into(),
                target_page_slug: "target".into(),
            }],
        )
        .await
        .expect("seed link");

    storage.pages().delete(source.id).await.expect("drop page");

    let slice = storage
        .page_links()
        .list_backlinks_to("main", "target", None, 50)
        .await
        .expect("list");
    assert!(
        slice.items.is_empty(),
        "ON DELETE CASCADE should remove the page_links row"
    );
}

#[tokio::test]
async fn pagination_walks_all_sources_in_order() {
    let storage = fresh_storage().await;
    let ns = make_namespace("main");
    storage.namespaces().create(&ns).await.expect("seed ns");

    // Three sources, all pointing at the same target.
    let mut sources = Vec::new();
    for i in 0..3 {
        let page = make_page(ns.id, &format!("src-{i}"));
        storage.pages().create(&page).await.expect("seed");
        storage
            .page_links()
            .replace_for_source(
                page.id,
                &[PageLink {
                    source_page_id: page.id,
                    target_namespace_slug: "main".into(),
                    target_page_slug: "hub".into(),
                }],
            )
            .await
            .expect("link");
        sources.push(page.id);
    }

    // Walk with limit=1.
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let slice = storage
            .page_links()
            .list_backlinks_to("main", "hub", cursor.clone(), 1)
            .await
            .expect("list");
        for item in slice.items {
            seen.push(item.source_page_id);
        }
        if slice.next.is_none() {
            break;
        }
        cursor = slice.next;
    }
    assert_eq!(seen.len(), 3, "saw: {seen:?}");
    // Pages are sorted by `id ASC` — UUIDv7 ids monotonically increase with
    // creation time, so the visit order tracks insertion order.
    assert_eq!(seen, sources);
}
