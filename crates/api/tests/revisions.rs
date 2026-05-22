//! Integration tests for the revision listing + diff endpoints (#10).
//!
//! Each test boots a fresh in-memory SQLite, applies migrations, seeds the
//! `Main` namespace plus a tester user, creates a page, applies a series of
//! `PUT` edits to build a 3-revision history (A -> B -> C), and drives the
//! router via `tower::ServiceExt::oneshot`. No TCP listener is bound.
//!
//! Decision: `GET .../revisions` for an unknown page returns **404** (not
//! an empty 200). This mirrors how `GET /api/v1/pages/{slug}` already
//! signals "no such page" and keeps clients from silently rendering an empty
//! history for a typo'd slug.

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

/// Bring up a fresh router + storage handle backed by in-memory SQLite, with
/// the `Main` namespace + a tester user pre-seeded.
async fn fresh_app() -> (Router, UserId) {
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

    // Revisions tests seed page edits through the page handlers; opt into
    // anonymous edits so the existing assertions keep working without
    // building a session flow per test.
    let mut auth_cfg = thewiki_api::config::Config::defaults().auth;
    auth_cfg.anonymous_edits = true;
    let state = AppState::new(storage, auth_cfg);
    let router = app::build_with_state(state);
    (router, user.id)
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
            "content": "alpha\nbeta\ngamma\n"
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
            "content": "alpha\nBETA\ngamma\n",
            "edit_summary": "shout the middle line"
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
            "content": "alpha\nBETA\ngamma\ndelta\n",
            "edit_summary": "add tail"
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
async fn list_revisions_returns_newest_first() {
    let (router, user_id) = fresh_app().await;
    let (rev_a, rev_b, rev_c) = seed_three_revisions(&router, user_id).await;

    let (status, body) =
        json_request(router, "GET", "/api/v1/pages/home/revisions", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 3, "expected three revisions, got: {body}");

    // Newest first: C, B, A.
    assert_eq!(items[0]["id"], rev_c);
    assert_eq!(items[1]["id"], rev_b);
    assert_eq!(items[2]["id"], rev_a);

    // Edit summaries propagate.
    assert_eq!(items[0]["edit_summary"], "add tail");
    assert_eq!(items[1]["edit_summary"], "shout the middle line");
    assert!(
        items[2]["edit_summary"].is_null(),
        "initial revision has no summary"
    );

    // Body excerpts surfaced.
    assert!(
        items[0]["body_excerpt"]
            .as_str()
            .expect("excerpt")
            .contains("delta")
    );

    // First page exhausted -> no cursor.
    assert!(body["next_cursor"].is_null());
}

#[tokio::test]
async fn list_revisions_for_unknown_page_returns_404() {
    let (router, _) = fresh_app().await;
    let (status, body) = json_request(
        router,
        "GET",
        "/api/v1/pages/no-such-page/revisions",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn diff_a_to_b_shows_single_line_change() {
    let (router, user_id) = fresh_app().await;
    let (rev_a, rev_b, _rev_c) = seed_three_revisions(&router, user_id).await;

    let uri = format!("/api/v1/pages/home/diff?from={rev_a}&to={rev_b}");
    let (status, body) = json_request(router, "GET", &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    assert_eq!(body["from"], rev_a);
    assert_eq!(body["to"], rev_b);

    let unified = body["unified"].as_str().expect("unified string");
    assert!(
        unified.contains("-beta"),
        "unified missing '-beta':\n{unified}"
    );
    assert!(
        unified.contains("+BETA"),
        "unified missing '+BETA':\n{unified}"
    );

    let hunks = body["hunks"].as_array().expect("hunks array");
    assert_eq!(hunks.len(), 1, "expected exactly one hunk: {body}");
    let hunk = &hunks[0];
    assert_eq!(hunk["old_start"], 1);
    assert_eq!(hunk["new_start"], 1);

    // Exactly one deletion + one insertion line in the hunk.
    let lines = hunk["lines"].as_array().expect("lines array");
    let deletions: Vec<_> = lines.iter().filter(|l| l["kind"] == "deletion").collect();
    let insertions: Vec<_> = lines.iter().filter(|l| l["kind"] == "insertion").collect();
    assert_eq!(deletions.len(), 1);
    assert_eq!(insertions.len(), 1);
    assert!(
        deletions[0]["content"]
            .as_str()
            .expect("content")
            .contains("beta")
    );
    assert!(
        insertions[0]["content"]
            .as_str()
            .expect("content")
            .contains("BETA")
    );
}

#[tokio::test]
async fn diff_a_to_c_reflects_three_way_history() {
    let (router, user_id) = fresh_app().await;
    let (rev_a, _rev_b, rev_c) = seed_three_revisions(&router, user_id).await;

    let uri = format!("/api/v1/pages/home/diff?from={rev_a}&to={rev_c}");
    let (status, body) = json_request(router, "GET", &uri, None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let unified = body["unified"].as_str().expect("unified string");
    // The diff between A (alpha/beta/gamma) and C (alpha/BETA/gamma/delta)
    // should reflect *both* the B-edit (beta -> BETA) and the C-edit (+delta).
    assert!(
        unified.contains("-beta"),
        "A->C missing '-beta':\n{unified}"
    );
    assert!(
        unified.contains("+BETA"),
        "A->C missing '+BETA':\n{unified}"
    );
    assert!(
        unified.contains("+delta"),
        "A->C missing '+delta':\n{unified}"
    );

    // Same assertions against the structured hunks.
    let hunks = body["hunks"].as_array().expect("hunks array");
    let all_lines: Vec<&Value> = hunks
        .iter()
        .flat_map(|h| h["lines"].as_array().expect("lines").iter())
        .collect();
    let deletions: Vec<&Value> = all_lines
        .iter()
        .copied()
        .filter(|l| l["kind"] == "deletion")
        .collect();
    let insertions: Vec<&Value> = all_lines
        .iter()
        .copied()
        .filter(|l| l["kind"] == "insertion")
        .collect();
    assert!(
        deletions
            .iter()
            .any(|l| l["content"].as_str().expect("content").contains("beta")),
        "expected 'beta' deletion in: {deletions:?}"
    );
    assert!(
        insertions
            .iter()
            .any(|l| l["content"].as_str().expect("content").contains("BETA")),
        "expected 'BETA' insertion in: {insertions:?}"
    );
    assert!(
        insertions
            .iter()
            .any(|l| l["content"].as_str().expect("content").contains("delta")),
        "expected 'delta' insertion in: {insertions:?}"
    );
}

#[tokio::test]
async fn diff_b_to_a_inverts_insertions_and_deletions() {
    let (router, user_id) = fresh_app().await;
    let (rev_a, rev_b, _rev_c) = seed_three_revisions(&router, user_id).await;

    let forward_uri = format!("/api/v1/pages/home/diff?from={rev_a}&to={rev_b}");
    let (status, forward) = json_request(router.clone(), "GET", &forward_uri, None, None).await;
    assert_eq!(status, StatusCode::OK);

    let reverse_uri = format!("/api/v1/pages/home/diff?from={rev_b}&to={rev_a}");
    let (status, reverse) = json_request(router, "GET", &reverse_uri, None, None).await;
    assert_eq!(status, StatusCode::OK);

    // Reversing the args flips deletion <-> insertion lines.
    let collect_kind = |body: &Value, kind: &str| -> Vec<String> {
        body["hunks"]
            .as_array()
            .expect("hunks")
            .iter()
            .flat_map(|h| h["lines"].as_array().expect("lines").iter())
            .filter(|l| l["kind"] == kind)
            .map(|l| l["content"].as_str().expect("content").to_string())
            .collect()
    };
    let forward_deletions = collect_kind(&forward, "deletion");
    let forward_insertions = collect_kind(&forward, "insertion");
    let reverse_deletions = collect_kind(&reverse, "deletion");
    let reverse_insertions = collect_kind(&reverse, "insertion");

    assert_eq!(forward_deletions, reverse_insertions);
    assert_eq!(forward_insertions, reverse_deletions);
}

#[tokio::test]
async fn diff_with_revision_from_other_page_returns_404() {
    let (router, user_id) = fresh_app().await;
    let (rev_a, _, _) = seed_three_revisions(&router, user_id).await;

    // Make a second page with its own revision, then try to diff revision A
    // (from `home`) against that page's slug.
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
    let other_rev = other["current_revision_id"]
        .as_str()
        .expect("other revision id")
        .to_string();

    // `from` belongs to `home` but the route is for `other` -> 404.
    let uri = format!("/api/v1/pages/other/diff?from={rev_a}&to={other_rev}");
    let (status, body) = json_request(router, "GET", &uri, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn diff_with_unknown_revision_returns_404() {
    let (router, user_id) = fresh_app().await;
    let (rev_a, _, _) = seed_three_revisions(&router, user_id).await;
    // A freshly-minted UUIDv7 that has never been written.
    let bogus = thewiki_core::RevisionId::new();

    let uri = format!("/api/v1/pages/home/diff?from={rev_a}&to={bogus}");
    let (status, body) = json_request(router, "GET", &uri, None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn diff_with_malformed_revision_id_returns_400() {
    let (router, user_id) = fresh_app().await;
    let (rev_a, _, _) = seed_three_revisions(&router, user_id).await;
    // `to=` is not a UUID -> axum's `Query` extractor rejects the request
    // with a plain-text body. We only care about the status here.
    let uri = format!("/api/v1/pages/home/diff?from={rev_a}&to=not-a-uuid");
    let request = Request::builder()
        .method("GET")
        .uri(&uri)
        .body(Body::empty())
        .expect("build request");
    let response = router.oneshot(request).await.expect("router responded");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn revisions_endpoint_exposed_in_openapi() {
    let (router, _) = fresh_app().await;
    let (status, body) = json_request(router, "GET", "/api/openapi.json", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let paths = body["paths"].as_object().expect("paths object");
    assert!(
        paths.contains_key("/api/v1/pages/{slug}/revisions"),
        "openapi paths missing revisions endpoint, got: {:?}",
        paths.keys().collect::<Vec<_>>()
    );
    assert!(
        paths.contains_key("/api/v1/pages/{slug}/diff"),
        "openapi paths missing diff endpoint, got: {:?}",
        paths.keys().collect::<Vec<_>>()
    );
}
