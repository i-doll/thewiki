//! Integration tests for the page CRUD endpoints.
//!
//! Each test boots a fresh in-memory SQLite, applies migrations, seeds the
//! default namespace (and any users referenced by `X-User-Id`), then drives
//! the router via `tower::ServiceExt::oneshot`. No TCP listener is bound.

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

/// Build a fresh router backed by a brand-new in-memory SQLite, with the
/// `Main` namespace and a known user (`X-User-Id` below) pre-seeded.
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

    // Pages tests opt into anonymous edits so the existing assertions (which
    // predate the configurable-auth wiring in #14 and used the now-gone
    // `x-user-id` header) keep exercising the create/update/delete paths.
    // The strict 401-without-session case is covered by the dedicated
    // `configurable_auth` integration test module.
    let mut auth_cfg = thewiki_api::config::Config::defaults().auth;
    auth_cfg.anonymous_edits = true;
    let state = AppState::new(storage, auth_cfg);
    let router = app::build_with_state(state);
    (router, user.id)
}

/// `fresh_app` variant that hands back the storage handle too, for tests
/// that want to assert against the database directly.
async fn fresh_app_with_storage() -> (Router, UserId, SqliteStorage) {
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

    let mut auth_cfg = thewiki_api::config::Config::defaults().auth;
    auth_cfg.anonymous_edits = true;
    let state = AppState::new(storage.clone(), auth_cfg);
    let router = app::build_with_state(state);
    (router, user.id, storage)
}

/// Send a request and parse the JSON body. Asserts the status code matches.
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

#[tokio::test]
async fn create_then_get_round_trip() {
    let (router, user_id) = fresh_app().await;

    let (status, created) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "# Hello"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create body: {created}");
    assert_eq!(created["slug"], "home");
    assert_eq!(created["title"], "Home");
    assert_eq!(created["content"], "# Hello");
    assert_eq!(created["namespace_slug"], "Main");
    assert!(created["current_revision_id"].is_string());

    let (status, fetched) = json_request(router, "GET", "/api/v1/pages/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "get body: {fetched}");
    assert_eq!(fetched["slug"], "home");
    assert_eq!(fetched["content"], "# Hello");
    assert_eq!(fetched["id"], created["id"]);
}

#[tokio::test]
async fn create_with_missing_namespace_returns_404() {
    let (router, user_id) = fresh_app().await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Nowhere",
            "slug": "home",
            "title": "Home",
            "content": "."
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn create_with_duplicate_slug_returns_409() {
    let (router, user_id) = fresh_app().await;
    let payload = json!({
        "namespace_slug": "Main",
        "slug": "home",
        "title": "Home",
        "content": "."
    });

    let (first, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(payload.clone()),
    )
    .await;
    assert_eq!(first, StatusCode::CREATED);

    let (second, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(payload),
    )
    .await;
    assert_eq!(second, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(body["code"], "conflict");
}

#[tokio::test]
async fn update_creates_a_new_revision() {
    let (router, user_id, storage) = fresh_app_with_storage().await;

    let (status, created) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "v1"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {created}");
    let initial_rev = created["current_revision_id"]
        .as_str()
        .expect("revision id");

    let (status, updated) = json_request(
        router,
        "PUT",
        "/api/v1/pages/home",
        Some(user_id),
        Some(json!({
            "content": "v2",
            "edit_summary": "second pass"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {updated}");
    let new_rev = updated["current_revision_id"]
        .as_str()
        .expect("revision id");
    assert_ne!(initial_rev, new_rev, "expected a new revision id");
    assert_eq!(updated["content"], "v2");

    // Verify directly against storage: page should now have two revisions in
    // its history.
    let page_id_str = updated["id"].as_str().expect("page id");
    let page_id =
        thewiki_core::PageId::from_uuid(uuid::Uuid::parse_str(page_id_str).expect("uuid"));
    let history = storage
        .revisions()
        .list_for_page(page_id, None, 10)
        .await
        .expect("list revisions");
    assert_eq!(history.items.len(), 2, "expected two revisions in history");
}

#[tokio::test]
async fn delete_then_get_returns_404() {
    let (router, user_id) = fresh_app().await;

    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "."
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = json_request(
        router.clone(),
        "DELETE",
        "/api/v1/pages/home",
        Some(user_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = json_request(router, "GET", "/api/v1/pages/home", None, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
}

#[tokio::test]
async fn list_with_no_pages_returns_empty() {
    let (router, _) = fresh_app().await;
    let (status, body) = json_request(router, "GET", "/api/v1/pages", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["items"].as_array().expect("items").len(), 0);
    assert!(body["next_cursor"].is_null());
}

#[tokio::test]
async fn list_paginates_with_cursor() {
    let (router, user_id) = fresh_app().await;

    // Seed five pages. Stagger the creation timestamps minimally by awaiting
    // each create — the SQLite cursor sorts on `(created_at, id)` and UUIDv7
    // is monotonic-by-time, so this gives a deterministic order.
    for i in 0..5 {
        let (status, _) = json_request(
            router.clone(),
            "POST",
            "/api/v1/pages",
            Some(user_id),
            Some(json!({
                "namespace_slug": "Main",
                "slug": format!("page-{i}"),
                "title": format!("Page {i}"),
                "content": format!("body {i}")
            })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    let (status, page1) =
        json_request(router.clone(), "GET", "/api/v1/pages?limit=2", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page1["items"].as_array().expect("items").len(), 2);
    let cursor1 = page1["next_cursor"]
        .as_str()
        .expect("first next_cursor")
        .to_string();

    let (status, page2) = json_request(
        router.clone(),
        "GET",
        &format!("/api/v1/pages?limit=2&cursor={}", urlencoding(&cursor1)),
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
        &format!("/api/v1/pages?limit=2&cursor={}", urlencoding(&cursor2)),
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
}

#[tokio::test]
async fn post_without_session_when_anonymous_edits_enabled_succeeds() {
    // The fresh_app fixture enables `anonymous_edits = true` (see the helper
    // above), so a POST without a session is accepted and credited to the
    // lazily-provisioned anonymous user. The strict 401-when-anonymous-edits-
    // disabled case is covered by the configurable-auth integration tests.
    let (router, _) = fresh_app().await;
    let (status, _body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        None,
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "."
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn get_without_auth_is_open() {
    let (router, user_id) = fresh_app().await;

    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "."
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = json_request(router, "GET", "/api/v1/pages/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["slug"], "home");
}

#[tokio::test]
async fn openapi_json_is_served() {
    let (router, _) = fresh_app().await;
    let (status, body) = json_request(router, "GET", "/api/openapi.json", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["info"]["title"], "thewiki API");
    let paths = body["paths"].as_object().expect("paths object");
    assert!(
        paths.contains_key("/api/v1/pages"),
        "openapi paths missing /api/v1/pages, got: {:?}",
        paths.keys().collect::<Vec<_>>()
    );
    assert!(paths.contains_key("/api/v1/pages/{slug}"));
}

/// Minimal `%`-encoder for cursor query params. Cursors are
/// `<rfc3339>|<hex>` and the colon in the RFC3339 timestamp would otherwise
/// be ambiguous as a URI delimiter.
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
