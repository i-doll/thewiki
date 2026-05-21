//! Integration tests for the recent-changes feed endpoint.
//!
//! Each test boots a fresh in-memory SQLite, seeds the default namespace plus
//! any extras a test needs, then drives the router through
//! `tower::ServiceExt::oneshot`. No listener is bound.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use thewiki_api::AppState;
use thewiki_api::app;
use thewiki_core::{EmailAddress, Namespace, NamespaceId, NamespaceSlug, User, UserId, Username};
use thewiki_storage::repo::{NamespaceRepository, UserRepository};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

/// Build a fresh router backed by a brand-new in-memory SQLite, with `Main`
/// pre-seeded and a known user (`X-User-Id` below) ready to author edits.
async fn fresh_app() -> (Router, UserId, SqliteStorage) {
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

    let namespace = Namespace {
        id: NamespaceId::new(),
        slug: NamespaceSlug::new("Main").expect("valid slug"),
        display_name: "Main".into(),
    };
    storage
        .namespaces()
        .create(&namespace)
        .await
        .expect("seed Main namespace");

    let user = User {
        id: UserId::new(),
        username: Username::new("tester").expect("valid username"),
        email: Some(EmailAddress::new("tester@example.com").expect("valid email")),
        display_name: Some("Tester".into()),
        created_at: OffsetDateTime::now_utc(),
        last_login_at: None,
    };
    storage
        .users()
        .create(&user, None)
        .await
        .expect("seed test user");

    let state = AppState::new(storage.clone());
    let router = app::build_with_state(state);
    (router, user.id, storage)
}

/// Seed an additional namespace, returning its slug for later use in URLs.
async fn add_namespace(storage: &SqliteStorage, slug: &str) {
    let ns = Namespace {
        id: NamespaceId::new(),
        slug: NamespaceSlug::new(slug).expect("valid slug"),
        display_name: slug.into(),
    };
    storage
        .namespaces()
        .create(&ns)
        .await
        .expect("seed namespace");
}

/// Send a JSON request and parse the response body.
async fn json_request(
    router: Router,
    method: &str,
    uri: &str,
    user_id: Option<UserId>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(uid) = user_id {
        builder = builder.header("x-user-id", uid.to_string());
    }
    let request = if let Some(body) = body {
        builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("build request")
    } else {
        builder.body(Body::empty()).expect("build request")
    };

    let response = router.oneshot(request).await.expect("router responded");
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
        let json: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("response wasn't JSON: {:?}", &bytes));
        (status, json)
    }
}

/// Helper for creating a page through the API. The route layer handles
/// committing the initial revision for us.
async fn create_page(
    router: Router,
    user_id: UserId,
    namespace_slug: &str,
    slug: &str,
    title: &str,
    content: &str,
) -> Value {
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": namespace_slug,
            "slug": slug,
            "title": title,
            "content": content,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create body: {body}");
    body
}

#[tokio::test]
async fn empty_feed_returns_empty_items_and_null_cursor() {
    let (router, _, _) = fresh_app().await;
    let (status, body) = json_request(router, "GET", "/api/v1/recent-changes", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["items"].as_array().expect("items").len(), 0);
    assert!(body["next_cursor"].is_null());
}

#[tokio::test]
async fn single_edit_appears_in_feed() {
    let (router, user_id, _) = fresh_app().await;
    create_page(router.clone(), user_id, "Main", "home", "Home", "# Hello").await;

    let (status, body) = json_request(router, "GET", "/api/v1/recent-changes", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "expected one entry, got: {body}");
    assert_eq!(items[0]["page_slug"], "home");
    assert_eq!(items[0]["namespace_slug"], "Main");
    assert_eq!(items[0]["author_username"], "tester");
    assert!(items[0]["revision_id"].is_string());
    assert!(items[0]["created_at"].is_string());
    assert!(body["next_cursor"].is_null());
}

#[tokio::test]
async fn multi_page_feed_is_newest_first() {
    let (router, user_id, storage) = fresh_app().await;
    add_namespace(&storage, "Help").await;

    // Create 5 pages across the two namespaces. UUIDv7 + sequential awaits
    // give a deterministic creation order; the feed reads it back newest
    // first so the last created should be first.
    let pages = [
        ("Main", "a", "A"),
        ("Help", "b", "B"),
        ("Main", "c", "C"),
        ("Help", "d", "D"),
        ("Main", "e", "E"),
    ];
    for (ns, slug, title) in pages {
        create_page(router.clone(), user_id, ns, slug, title, "body").await;
    }

    let (status, body) = json_request(router, "GET", "/api/v1/recent-changes", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 5);
    let slugs: Vec<&str> = items
        .iter()
        .map(|i| i["page_slug"].as_str().expect("slug"))
        .collect();
    assert_eq!(slugs, vec!["e", "d", "c", "b", "a"], "newest first");
}

#[tokio::test]
async fn filter_by_namespace_excludes_other_namespaces() {
    let (router, user_id, storage) = fresh_app().await;
    add_namespace(&storage, "Help").await;

    create_page(router.clone(), user_id, "Main", "m1", "M1", "x").await;
    create_page(router.clone(), user_id, "Help", "h1", "H1", "y").await;
    create_page(router.clone(), user_id, "Main", "m2", "M2", "z").await;

    let (status, body) = json_request(
        router,
        "GET",
        "/api/v1/recent-changes?namespace=Help",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["namespace_slug"], "Help");
    assert_eq!(items[0]["page_slug"], "h1");
}

#[tokio::test]
async fn since_filter_excludes_future_and_includes_past() {
    let (router, user_id, _) = fresh_app().await;
    create_page(router.clone(), user_id, "Main", "p1", "P1", "x").await;
    create_page(router.clone(), user_id, "Main", "p2", "P2", "y").await;

    // `since` set to the far future yields nothing.
    let (status, body) = json_request(
        router.clone(),
        "GET",
        "/api/v1/recent-changes?since=2999-01-01T00:00:00Z",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["items"].as_array().expect("items").len(), 0);

    // `since` set to the unix epoch yields both edits.
    let (status, body) = json_request(
        router,
        "GET",
        "/api/v1/recent-changes?since=1970-01-01T00:00:00Z",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["items"].as_array().expect("items").len(), 2);
}

#[tokio::test]
async fn pagination_walks_via_cursor() {
    let (router, user_id, _) = fresh_app().await;
    for i in 0..5 {
        create_page(
            router.clone(),
            user_id,
            "Main",
            &format!("p-{i}"),
            &format!("P{i}"),
            "body",
        )
        .await;
    }

    let (status, page1) = json_request(
        router.clone(),
        "GET",
        "/api/v1/recent-changes?limit=2",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page1["items"].as_array().expect("items").len(), 2);
    let cursor1 = page1["next_cursor"]
        .as_str()
        .expect("first next_cursor")
        .to_string();

    let (status, page2) = json_request(
        router.clone(),
        "GET",
        &format!(
            "/api/v1/recent-changes?limit=2&cursor={}",
            urlencoding(&cursor1)
        ),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page2["items"].as_array().expect("items").len(), 2);
    let cursor2 = page2["next_cursor"]
        .as_str()
        .expect("second next_cursor")
        .to_string();

    let (status, page3) = json_request(
        router,
        "GET",
        &format!(
            "/api/v1/recent-changes?limit=2&cursor={}",
            urlencoding(&cursor2)
        ),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page3["items"].as_array().expect("items").len(), 1);
    assert!(
        page3["next_cursor"].is_null(),
        "final page should not advertise more, got: {page3}"
    );

    // The walk should have surfaced every original page exactly once.
    let collect_slugs = |body: &Value| -> Vec<String> {
        body["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|i| i["page_slug"].as_str().expect("slug").to_string())
            .collect()
    };
    let mut all = collect_slugs(&page1);
    all.extend(collect_slugs(&page2));
    all.extend(collect_slugs(&page3));
    all.sort();
    assert_eq!(
        all,
        vec!["p-0", "p-1", "p-2", "p-3", "p-4"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn cursor_is_stable_across_new_edits() {
    let (router, user_id, _) = fresh_app().await;
    for i in 0..3 {
        create_page(
            router.clone(),
            user_id,
            "Main",
            &format!("p-{i}"),
            &format!("P{i}"),
            "body",
        )
        .await;
    }

    // First page (newest entry) captures the cursor BEFORE we insert a new
    // edit. The cursor encodes a fixed `(timestamp, id)` boundary, so the
    // follow-up call should still yield the older entries it was iterating —
    // never the edit that landed afterwards.
    let (status, page1) = json_request(
        router.clone(),
        "GET",
        "/api/v1/recent-changes?limit=1",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let first_slugs: Vec<String> = page1["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["page_slug"].as_str().expect("slug").to_string())
        .collect();
    assert_eq!(first_slugs, vec!["p-2".to_string()]);
    let cursor1 = page1["next_cursor"]
        .as_str()
        .expect("first next_cursor")
        .to_string();

    // New edit lands after we captured the cursor.
    create_page(router.clone(), user_id, "Main", "p-late", "Late", "fresh").await;

    // Following the cursor must NOT show the newly inserted entry — it is
    // newer than the cursor boundary. We expect to walk the older pages
    // (`p-1`, `p-0`).
    let (status, page2) = json_request(
        router.clone(),
        "GET",
        &format!(
            "/api/v1/recent-changes?limit=10&cursor={}",
            urlencoding(&cursor1)
        ),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let later_slugs: Vec<String> = page2["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["page_slug"].as_str().expect("slug").to_string())
        .collect();
    assert_eq!(
        later_slugs,
        vec!["p-1".to_string(), "p-0".to_string()],
        "cursor-based follow-up should yield older entries only"
    );
    assert!(page2["next_cursor"].is_null());

    // Sanity: a fresh listing (no cursor) DOES surface the new edit at the
    // top, confirming it really did land.
    let (status, fresh) =
        json_request(router, "GET", "/api/v1/recent-changes?limit=10", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let fresh_slugs: Vec<String> = fresh["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["page_slug"].as_str().expect("slug").to_string())
        .collect();
    assert_eq!(fresh_slugs[0], "p-late");
}

#[tokio::test]
async fn unknown_namespace_filter_returns_404() {
    let (router, _, _) = fresh_app().await;
    let (status, body) = json_request(
        router,
        "GET",
        "/api/v1/recent-changes?namespace=Nowhere",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn filter_by_actor_excludes_other_authors() {
    let (router, alice_id, storage) = fresh_app().await;

    let bob = User {
        id: UserId::new(),
        username: Username::new("bob").expect("valid username"),
        email: None,
        display_name: None,
        created_at: OffsetDateTime::now_utc(),
        last_login_at: None,
    };
    storage.users().create(&bob, None).await.expect("seed bob");

    create_page(
        router.clone(),
        alice_id,
        "Main",
        "alice-page",
        "Alice",
        "by Alice",
    )
    .await;
    create_page(router.clone(), bob.id, "Main", "bob-page", "Bob", "by Bob").await;

    let (status, body) = json_request(
        router,
        "GET",
        "/api/v1/recent-changes?actor=bob",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items array");
    assert_eq!(
        items.len(),
        1,
        "expected one bob-authored change: {items:?}"
    );
    assert_eq!(items[0]["author_username"], "bob");
    assert_eq!(items[0]["page_slug"], "bob-page");
}

#[tokio::test]
async fn unknown_actor_filter_returns_404() {
    let (router, _, _) = fresh_app().await;
    let (status, body) = json_request(
        router,
        "GET",
        "/api/v1/recent-changes?actor=ghost",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn openapi_includes_recent_changes_path() {
    let (router, _, _) = fresh_app().await;
    let (status, body) = json_request(router, "GET", "/api/openapi.json", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let paths = body["paths"].as_object().expect("paths object");
    assert!(
        paths.contains_key("/api/v1/recent-changes"),
        "openapi paths missing /api/v1/recent-changes, got: {:?}",
        paths.keys().collect::<Vec<_>>()
    );
}

/// Minimal `%`-encoder for cursor query params (`<rfc3339>|<hex>` contains
/// `:` and `|` which would be ambiguous if not encoded).
fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
