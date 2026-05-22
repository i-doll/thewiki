//! Integration tests for the page index.
//!
//! Each test opens a fresh Tantivy index in a `tempfile::TempDir`, drives it
//! through the synchronous `SearchIndex` surface (the async indexer is
//! covered by the API layer's integration tests where the runtime is
//! already in scope), and asserts ranked + filtered behaviour.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use tempfile::TempDir;
use thewiki_core::{NamespaceId, PageId};
use thewiki_search::{PageDoc, SearchIndex, SearchQuery};
use time::OffsetDateTime;

/// Build a [`PageDoc`] with a deterministic body so the assertions below
/// don't fight randomly-generated text.
fn doc(title: &str, body: &str) -> PageDoc {
    PageDoc {
        page_id: PageId::new(),
        namespace_id: NamespaceId::new(),
        namespace_slug: "main".to_string(),
        slug: slug_from(title),
        title: title.to_string(),
        body: body.to_string(),
        tags: Vec::new(),
        updated_at: OffsetDateTime::now_utc(),
    }
}

fn slug_from(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Open an index in `path`, apply `body`, commit, and return the reopened
/// `SearchIndex` for assertions. Keeping the closure body separate from the
/// commit/reopen dance avoids each test repeating the boilerplate.
fn with_index<F>(path: &Path, body: F) -> SearchIndex
where
    F: FnOnce(&SearchIndex, &mut tantivy::IndexWriter<tantivy::TantivyDocument>),
{
    let index = SearchIndex::open(path).expect("open index");
    let mut writer = index.new_writer().expect("writer");
    body(&index, &mut writer);
    writer.commit().expect("commit");
    index.write_last_indexed_marker().expect("marker");
    SearchIndex::open(path).expect("reopen")
}

#[test]
fn upsert_and_search_by_title() {
    let dir = TempDir::new().expect("tmpdir");
    let alpha = doc("Alpha Centauri", "the closest star system to ours");
    let beta = doc("Beta Pictoris", "a famous debris-disc star");
    let gamma = doc("Gamma Cassiopeiae", "a hot blue variable in Cassiopeia");

    let index = with_index(dir.path(), |idx, w| {
        idx.upsert_on(w, &alpha).unwrap();
        idx.upsert_on(w, &beta).unwrap();
        idx.upsert_on(w, &gamma).unwrap();
    });

    let res = index
        .search(&SearchQuery::text("centauri", 10))
        .expect("search");
    assert!(!res.hits.is_empty(), "expected at least one hit");
    let top = &res.hits[0];
    assert_eq!(top.page_id, alpha.page_id, "top hit must be Alpha");
    assert_eq!(top.title, "Alpha Centauri");
}

#[test]
fn updating_doc_overwrites_old_content() {
    let dir = TempDir::new().expect("tmpdir");
    let mut alpha = doc("Alpha Centauri", "version one of the body");

    let index = with_index(dir.path(), |idx, w| {
        idx.upsert_on(w, &alpha).unwrap();
    });
    let res = index.search(&SearchQuery::text("version one", 10)).unwrap();
    assert!(
        res.hits.iter().any(|h| h.page_id == alpha.page_id),
        "first version must be findable"
    );

    // Re-upsert with the same page_id but a new body.
    alpha.body = "now version two with completely different prose".to_string();
    let index = with_index(dir.path(), |idx, w| {
        idx.upsert_on(w, &alpha).unwrap();
    });

    // The unique-to-v1 token is gone — searching for it must not find the
    // page at all. ("version" appears in both bodies; the discriminator is
    // the rest of the v1 phrase, "version one of the body".)
    let stale = index
        .search(&SearchQuery::text("\"version one\"", 10))
        .unwrap();
    assert!(
        !stale.hits.iter().any(|h| h.page_id == alpha.page_id),
        "the old body must no longer match the v1 phrase"
    );
    let fresh = index
        .search(&SearchQuery::text("\"version two\"", 10))
        .unwrap();
    assert!(
        fresh.hits.iter().any(|h| h.page_id == alpha.page_id),
        "the new body must be searchable"
    );
}

#[test]
fn deleting_doc_removes_it_from_results() {
    let dir = TempDir::new().expect("tmpdir");
    let alpha = doc("Alpha Centauri", "the closest star system to ours");
    let beta = doc("Beta Pictoris", "a famous debris-disc star");

    let index = with_index(dir.path(), |idx, w| {
        idx.upsert_on(w, &alpha).unwrap();
        idx.upsert_on(w, &beta).unwrap();
    });
    let res = index.search(&SearchQuery::text("star", 10)).unwrap();
    assert!(res.hits.len() >= 2, "both stars indexed");

    let index = with_index(dir.path(), |idx, w| {
        idx.delete_on(w, alpha.page_id).unwrap();
    });
    let res = index.search(&SearchQuery::text("centauri", 10)).unwrap();
    assert!(
        !res.hits.iter().any(|h| h.page_id == alpha.page_id),
        "deleted page must not appear in hits"
    );
}

#[test]
fn snippet_contains_highlight_markers() {
    let dir = TempDir::new().expect("tmpdir");
    let page = doc(
        "Quasars and AGN",
        "Quasars are extremely luminous active galactic nuclei powered by accretion onto a supermassive black hole.",
    );

    let index = with_index(dir.path(), |idx, w| {
        idx.upsert_on(w, &page).unwrap();
    });
    let res = index
        .search(&SearchQuery::text("luminous", 10))
        .expect("search");
    let hit = res.hits.first().expect("at least one hit");
    assert!(
        hit.snippet.contains("<mark>") && hit.snippet.contains("</mark>"),
        "snippet should carry highlight markers; got {:?}",
        hit.snippet
    );
}

#[test]
fn crash_recovery_marker_round_trips() {
    let dir = TempDir::new().expect("tmpdir");
    let page = doc("Crash Recovery", "indexer durability sanity check");

    let index = with_index(dir.path(), |idx, w| {
        idx.upsert_on(w, &page).unwrap();
    });
    assert!(
        index.has_last_indexed_marker(),
        "marker must be present after commit"
    );

    // Simulate a crash by wiping the marker file but keeping the segments.
    let marker = dir.path().join(".last_indexed");
    std::fs::remove_file(&marker).expect("remove marker");

    let reopened = SearchIndex::open(dir.path()).expect("reopen after crash");
    assert!(
        !reopened.has_last_indexed_marker(),
        "marker must be gone after simulated crash"
    );

    // The data is still searchable (Tantivy commits are durable on their
    // own; the marker only signals "no rebuild needed"). The caller's
    // recovery path is to enqueue a Rebuild + replay, which the indexer
    // worker handles.
    let hits = reopened
        .search(&SearchQuery::text("durability", 10))
        .expect("search");
    assert!(
        hits.hits.iter().any(|h| h.page_id == page.page_id),
        "data still searchable after simulated crash"
    );
}

#[test]
fn empty_query_returns_no_hits() {
    let dir = TempDir::new().expect("tmpdir");
    let alpha = doc("Alpha", "body");

    let index = with_index(dir.path(), |idx, w| {
        idx.upsert_on(w, &alpha).unwrap();
    });

    let res = index
        .search(&SearchQuery::text("", 10))
        .expect("empty search returns Ok with no hits");
    assert!(res.hits.is_empty());
    assert_eq!(res.total_estimate, 0);
}

#[test]
fn namespace_filter_excludes_other_namespaces() {
    let dir = TempDir::new().expect("tmpdir");
    let mut alpha = doc("Alpha Star", "main ns content");
    alpha.namespace_slug = "main".to_string();
    let mut help = doc("Help Page", "help ns content");
    help.namespace_slug = "help".to_string();

    let index = with_index(dir.path(), |idx, w| {
        idx.upsert_on(w, &alpha).unwrap();
        idx.upsert_on(w, &help).unwrap();
    });

    let mut q = SearchQuery::text("content", 10);
    q.namespace_slug = Some("help".into());
    let res = index.search(&q).unwrap();
    assert!(res.hits.iter().any(|h| h.page_id == help.page_id));
    assert!(
        !res.hits.iter().any(|h| h.page_id == alpha.page_id),
        "main-namespace page must be filtered out"
    );
}
