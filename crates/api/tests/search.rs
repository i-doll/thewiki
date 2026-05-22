//! Integration tests for `GET /api/v1/search` (#27).
//!
//! Each test:
//!
//! 1. Boots an in-memory SQLite-backed router (no auth required for reads).
//! 2. Opens a Tantivy index in a fresh `TempDir`.
//! 3. Seeds a handful of `PageDoc`s directly through the synchronous
//!    `SearchIndex::upsert_on` path so we don't have to wait on the async
//!    indexer worker.
//! 4. Drives the search endpoint via `tower::ServiceExt::oneshot`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tempfile::TempDir;
use thewiki_api::AppState;
use thewiki_api::app;
use thewiki_core::{Namespace, NamespaceId, NamespaceSlug, PageId};
use thewiki_search::{PageDoc, SearchIndex, Searcher};
use thewiki_storage::repo::NamespaceRepository;
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

struct Fixture {
    router: Router,
    _index_dir: TempDir,
}

/// Build a freshly-indexed router with the three pages used by every test
/// below pre-loaded into a temporary Tantivy index.
async fn fresh_app_with_seeded_index() -> Fixture {
    let storage = SqliteStorage::new(
        "sqlite::memory:",
        SqliteOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            foreign_keys: true,
        },
    )
    .await
    .expect("open + migrate in-memory sqlite");

    let ns = Namespace {
        id: NamespaceId::new(),
        slug: NamespaceSlug::new("Main").expect("valid slug"),
        display_name: "Main".into(),
    };
    storage.namespaces().create(&ns).await.expect("seed ns");

    // Open + seed the Tantivy index synchronously. The route handler reads
    // through the same `Arc<SearchIndex>` so the commit below is visible
    // through the reader by the time the test issues the request.
    let dir = TempDir::new().expect("tmpdir");
    let index = SearchIndex::open(dir.path()).expect("open index");
    let mut writer = index.new_writer().expect("writer");

    let pages = [
        (
            "Alpha Centauri",
            "alpha-centauri",
            "Alpha Centauri is the closest star system to the Sun.",
        ),
        (
            "Beta Pictoris",
            "beta-pictoris",
            "Beta Pictoris is a famous young debris-disc star.",
        ),
        (
            "Gamma Cassiopeiae",
            "gamma-cassiopeiae",
            "Gamma Cassiopeiae is a hot blue variable in Cassiopeia.",
        ),
    ];
    for (title, slug, body) in pages {
        let doc = PageDoc {
            page_id: PageId::new(),
            namespace_id: ns.id,
            namespace_slug: "Main".to_string(),
            slug: slug.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            tags: Vec::new(),
            updated_at: OffsetDateTime::now_utc(),
        };
        index.upsert_on(&writer, &doc).expect("upsert");
    }
    writer.commit().expect("commit");
    index.write_last_indexed_marker().expect("marker");

    // Reopen to pick up the freshly-committed segments through the cached
    // reader. The router then holds an `Arc<SearchIndex>` aliased into the
    // searcher.
    let index = Arc::new(SearchIndex::open(dir.path()).expect("reopen index"));
    let searcher = Searcher::new(Arc::clone(&index));

    let auth_cfg = thewiki_api::config::Config::defaults().auth;
    let state = AppState::new(storage, auth_cfg).with_searcher(searcher);
    let router = app::build_with_state(state);

    Fixture {
        router,
        _index_dir: dir,
    }
}

async fn get_search(router: Router, query: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(query)
        .body(Body::empty())
        .expect("build req");
    let response = router.oneshot(req).await.expect("router");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    if bytes.is_empty() {
        (status, Value::Null)
    } else {
        let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }
}

#[tokio::test]
async fn search_returns_ranked_hits_with_highlighted_snippets() {
    let fx = fresh_app_with_seeded_index().await;
    let (status, body) = get_search(fx.router, "/api/v1/search?q=closest").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items");
    assert!(
        !items.is_empty(),
        "expected at least one hit; got body: {body}"
    );
    let top = &items[0];
    assert_eq!(top["title"], "Alpha Centauri");
    let snippet = top["snippet"].as_str().expect("snippet");
    assert!(
        snippet.contains("<mark>") && snippet.contains("</mark>"),
        "snippet should contain <mark>…</mark>; got {snippet}"
    );
}

#[tokio::test]
async fn empty_query_returns_400() {
    let fx = fresh_app_with_seeded_index().await;
    let (status, body) = get_search(fx.router.clone(), "/api/v1/search?q=").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["code"], "invalid_input");
}

#[tokio::test]
async fn whitespace_only_query_returns_400() {
    let fx = fresh_app_with_seeded_index().await;
    let (status, body) = get_search(fx.router.clone(), "/api/v1/search?q=%20%20%20").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(body["code"], "invalid_input");
}

#[tokio::test]
async fn limit_clamps_to_50() {
    let fx = fresh_app_with_seeded_index().await;
    // 500 is over the cap; the handler should clamp silently. We can't
    // observe the clamp directly through the wire but we can confirm the
    // request still succeeds (anything > 50 used to be rejected by the
    // search crate's `with_limit` when the result set was smaller; today
    // the handler accepts and clamps).
    let (status, body) = get_search(fx.router.clone(), "/api/v1/search?q=star&limit=500").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body["items"].as_array().expect("items").len() <= 50,
        "expected <= 50 items, got body: {body}"
    );
}

#[tokio::test]
async fn namespace_filter_scopes_to_one_namespace() {
    let fx = fresh_app_with_seeded_index().await;
    // The fixture indexed everything under `Main`, so filtering to `Main`
    // is non-empty and filtering to a missing namespace returns nothing.
    let (status, body) =
        get_search(fx.router.clone(), "/api/v1/search?q=star&namespace=Main").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(!body["items"].as_array().expect("items").is_empty());

    let (status, body) = get_search(
        fx.router.clone(),
        "/api/v1/search?q=star&namespace=NoSuchNamespace",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body["items"].as_array().expect("items").is_empty());
}

#[tokio::test]
async fn cursor_round_trips_through_the_handler() {
    // Cursor pagination is reserved for a follow-up. Today the handler
    // accepts the parameter (so clients can pass it through opaquely) but
    // every response carries `next_cursor: null`. This test pins the
    // current wire contract.
    let fx = fresh_app_with_seeded_index().await;
    let (status, body) = get_search(fx.router, "/api/v1/search?q=star&cursor=opaque").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body["next_cursor"].is_null(),
        "next_cursor should be null today; got {}",
        body["next_cursor"]
    );
}

#[tokio::test]
async fn missing_q_parameter_returns_400() {
    let fx = fresh_app_with_seeded_index().await;
    let (status, _body) = get_search(fx.router, "/api/v1/search").await;
    // Axum's `Query` extractor rejects missing required fields with 400.
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
