//! Integration tests for the page revert endpoint (#11).
//!
//! Each test boots a fresh in-memory SQLite, applies migrations, seeds the
//! `Main` namespace plus a tester user, creates a page, builds a 3-revision
//! history (A -> B -> C), and drives the router via `tower::ServiceExt::oneshot`.
//! No TCP listener is bound.
//!
//! Decisions worth noting:
//!
//! * A revert that targets a revision belonging to a different page returns
//!   **404** (not 403). This matches the diff endpoint and stops the route
//!   from being used as a cross-page existence oracle for revision ids.
//! * The revert revision's `parent_id` is the page's *current* head, not the
//!   revision being reverted to. This keeps the history graph linear and
//!   makes the revert auditable as a discrete event between two specific
//!   revisions.

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
use thewiki_storage::repo::{NamespaceRepository, RevisionRepository, UserRepository};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

/// Bring up a fresh router + storage handle backed by in-memory SQLite, with
/// the `Main` namespace + a tester user pre-seeded. The storage handle is
/// returned alongside the router so tests can assert against the database
/// directly (e.g. revision parent pointers).
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

/// Send a JSON request and parse the response. Asserts nothing about the
/// status; the caller decides what's expected.
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

/// Seed a 3-revision history (A -> B -> C) on slug `home`. Returns the
/// revision ids in order (rev_a, rev_b, rev_c).
async fn seed_three_revisions(router: &Router, user_id: UserId) -> (String, String, String) {
    let (status, created) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "alpha\n"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create body: {created}");
    let rev_a = created["current_revision_id"]
        .as_str()
        .expect("revision A id")
        .to_string();

    let (status, updated_b) = json_request(
        router.clone(),
        "PUT",
        "/api/v1/pages/home",
        Some(user_id),
        Some(json!({
            "content": "alpha\nbeta\n",
            "edit_summary": "add beta"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update B body: {updated_b}");
    let rev_b = updated_b["current_revision_id"]
        .as_str()
        .expect("revision B id")
        .to_string();

    let (status, updated_c) = json_request(
        router.clone(),
        "PUT",
        "/api/v1/pages/home",
        Some(user_id),
        Some(json!({
            "content": "alpha\nbeta\ngamma\n",
            "edit_summary": "add gamma"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update C body: {updated_c}");
    let rev_c = updated_c["current_revision_id"]
        .as_str()
        .expect("revision C id")
        .to_string();

    assert_ne!(rev_a, rev_b);
    assert_ne!(rev_b, rev_c);
    (rev_a, rev_b, rev_c)
}

#[tokio::test]
async fn revert_to_a_creates_new_revision_with_a_body() {
    let (router, user_id, storage) = fresh_app().await;
    let (rev_a, _rev_b, rev_c) = seed_three_revisions(&router, user_id).await;

    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages/home/revert",
        Some(user_id),
        Some(json!({ "from_revision": rev_a })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    // 1. Response body shows the page now carries revision A's content.
    assert_eq!(body["content"], "alpha\n", "body: {body}");
    let new_rev = body["current_revision_id"]
        .as_str()
        .expect("revert revision id")
        .to_string();
    assert_ne!(
        new_rev, rev_a,
        "revert must create a new revision, not reuse A"
    );
    assert_ne!(
        new_rev, rev_c,
        "revert revision must be distinct from old head"
    );

    // 2. Listing now reveals four revisions: A, B, C, and the revert.
    let (status, list) = json_request(
        router.clone(),
        "GET",
        "/api/v1/pages/home/revisions",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "list body: {list}");
    let items = list["items"].as_array().expect("items");
    assert_eq!(
        items.len(),
        4,
        "expected four revisions after revert, got: {list}"
    );

    // 3. The revert revision's `parent_id` is C (the old head), NOT A. Probe
    //    storage directly — the list endpoint surfaces `parent_id`, so we can
    //    do this through HTTP too, but going to the DB keeps the assertion
    //    pinned to the storage invariant.
    let rev_uuid = thewiki_core::RevisionId::from_uuid(
        uuid::Uuid::parse_str(&new_rev).expect("revert revision id is a UUID"),
    );
    let stored = storage
        .revisions()
        .get_by_id(rev_uuid)
        .await
        .expect("load revert revision");
    let parent = stored.parent_id.expect("revert has a parent");
    assert_eq!(
        parent.to_string(),
        rev_c,
        "revert parent must be the old head C, not the revision being reverted to",
    );

    // 4. Default edit summary references the historical revision id.
    let expected_summary = format!("Reverted to {rev_a}");
    assert_eq!(
        stored.edit_summary.as_deref(),
        Some(expected_summary.as_str())
    );
}

#[tokio::test]
async fn revert_with_custom_message_uses_it_as_edit_summary() {
    let (router, user_id, storage) = fresh_app().await;
    let (rev_a, _rev_b, _rev_c) = seed_three_revisions(&router, user_id).await;

    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages/home/revert",
        Some(user_id),
        Some(json!({ "from_revision": rev_a, "message": "vandalism" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let new_rev = body["current_revision_id"]
        .as_str()
        .expect("revert revision id")
        .to_string();
    let rev_uuid = thewiki_core::RevisionId::from_uuid(
        uuid::Uuid::parse_str(&new_rev).expect("revert revision id is a UUID"),
    );
    let stored = storage
        .revisions()
        .get_by_id(rev_uuid)
        .await
        .expect("load revert revision");
    assert_eq!(stored.edit_summary.as_deref(), Some("vandalism"));
}

#[tokio::test]
async fn revert_with_unknown_revision_id_returns_404() {
    let (router, user_id, _storage) = fresh_app().await;
    // Build a page so the slug resolves; otherwise we'd be measuring the
    // page-not-found path instead of the revision-not-found path.
    let _ = seed_three_revisions(&router, user_id).await;
    let bogus = thewiki_core::RevisionId::new();

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages/home/revert",
        Some(user_id),
        Some(json!({ "from_revision": bogus.to_string() })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn revert_with_revision_from_other_page_returns_404() {
    let (router, user_id, _storage) = fresh_app().await;
    let (rev_a, _rev_b, _rev_c) = seed_three_revisions(&router, user_id).await;

    // Create a second page with its own revision history.
    let (status, other) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "other",
            "title": "Other",
            "content": "elsewhere\n"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {other}");

    // Try to revert `other` to revision A (which belongs to `home`). The
    // mismatch must map to 404 — not 403 — so the route can't be used to
    // probe for the existence of revisions on other pages.
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages/other/revert",
        Some(user_id),
        Some(json!({ "from_revision": rev_a })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn revert_without_user_id_returns_401() {
    let (router, user_id, _storage) = fresh_app().await;
    let (rev_a, _rev_b, _rev_c) = seed_three_revisions(&router, user_id).await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages/home/revert",
        // Crucially, no `X-User-Id` header.
        None,
        Some(json!({ "from_revision": rev_a })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(body["code"], "unauthenticated");
}

#[tokio::test]
async fn revert_endpoint_exposed_in_openapi() {
    let (router, _, _) = fresh_app().await;
    let (status, body) = json_request(router, "GET", "/api/openapi.json", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let paths = body["paths"].as_object().expect("paths object");
    assert!(
        paths.contains_key("/api/v1/pages/{slug}/revert"),
        "openapi paths missing revert endpoint, got: {:?}",
        paths.keys().collect::<Vec<_>>()
    );
}
