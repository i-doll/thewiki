//! Integration tests for the watchlist + Atom feed endpoints (#46).
//!
//! Each test boots a fresh in-memory SQLite, applies migrations, seeds the
//! default `Main` namespace plus the users / sessions the test needs, then
//! drives the router via `tower::ServiceExt::oneshot`. No TCP listener is
//! bound.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use thewiki_api::AppState;
use thewiki_api::app;
use thewiki_api::auth::password::Argon2Hasher;
use thewiki_api::auth::state::AuthState;
use thewiki_api::config::{Argon2Config, Config};
use thewiki_core::{
    EmailAddress, Namespace, NamespaceId, NamespaceSlug, ProtectionLevel, User, UserId, Username,
};
use thewiki_storage::repo::{
    AuditLogFilter, AuditLogRepository, NamespaceRepository, PageRepository, SessionRepository,
    UserRepository,
};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

/// Build a fresh router backed by a brand-new in-memory SQLite, with `Main`
/// pre-seeded, the "tester" user wired up, and an active session cookie ready
/// to author edits.
async fn fresh_app() -> (Router, SqliteStorage, UserId, String) {
    let storage = SqliteStorage::new(
        "sqlite::memory:",
        SqliteOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            foreign_keys: true,
        },
    )
    .await
    .expect("open + migrate sqlite");

    storage
        .namespaces()
        .create(&Namespace {
            id: NamespaceId::new(),
            slug: NamespaceSlug::new("Main").expect("valid slug"),
            display_name: "Main".into(),
        })
        .await
        .expect("seed Main");

    let user = User {
        id: UserId::new(),
        username: Username::new("tester").expect("valid username"),
        email: Some(EmailAddress::new("tester@example.com").expect("valid email")),
        display_name: Some("Tester".into()),
        created_at: OffsetDateTime::now_utc(),
        last_login_at: None,
    };
    storage.users().create(&user, None).await.expect("seed user");

    let auth_cfg = Config::defaults().auth;
    let hasher = Arc::new(
        Argon2Hasher::new(Argon2Config {
            memory_kib: 19_456,
            iterations: 2,
            parallelism: 1,
        })
        .expect("hasher"),
    );
    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        Duration::from_secs(60 * 60),
        false,
        auth_cfg.clone(),
    );
    let state = AppState::new(storage.clone(), auth_cfg).with_auth_state(auth_state);

    let mut rate_limit = Config::defaults().rate_limit;
    rate_limit.enabled = false;
    let router = app::build_with_state_with_rate_limit(state, rate_limit);

    let session = seed_session(&storage, user.id).await;
    (router, storage, user.id, session)
}

async fn seed_session(storage: &SqliteStorage, user_id: UserId) -> String {
    storage
        .sessions()
        .create(user_id, Duration::from_secs(60 * 60), None, None)
        .await
        .expect("seed session")
        .id
        .into_uuid()
        .to_string()
}

async fn request(
    router: Router,
    method: &str,
    uri: &str,
    session: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(s) = session {
        builder = builder.header(header::COOKIE, format!("thewiki_session={s}"));
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
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

async fn json_request(
    router: Router,
    method: &str,
    uri: &str,
    session: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, _, bytes) = request(router, method, uri, session, body).await;
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("json")
    };
    (status, value)
}

/// Create a page and return its `id` from the response.
async fn create_page(router: Router, session: &str, slug: &str) -> String {
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(session),
        Some(json!({
            "namespace_slug": "Main",
            "slug": slug,
            "title": slug,
            "content": format!("# {slug}"),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create body: {body}");
    body["id"].as_str().expect("page id").to_owned()
}

// ─── Watchlist CRUD ───────────────────────────────────────────────────────

#[tokio::test]
async fn watch_then_list_then_unwatch_round_trips() {
    let (router, _storage, _user_id, session) = fresh_app().await;
    let page_id = create_page(router.clone(), &session, "alpha").await;

    let (status, body) = json_request(router.clone(), "GET", "/api/v1/watchlist", Some(&session), None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["items"].as_array().expect("items").len(), 0);

    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/watchlist",
        Some(&session),
        Some(json!({ "page_id": page_id })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["watched"], true);

    let (status, body) = json_request(router.clone(), "GET", "/api/v1/watchlist", Some(&session), None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "items: {body}");
    assert_eq!(items[0]["page_id"], page_id);
    assert_eq!(items[0]["slug"], "alpha");
    assert_eq!(items[0]["namespace"], "Main");
    assert_eq!(items[0]["title"], "alpha");
    assert!(items[0]["watched_at"].is_string());

    // DELETE removes the row.
    let (status, _, _) = request(
        router.clone(),
        "DELETE",
        &format!("/api/v1/watchlist/{page_id}"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, body) = json_request(router, "GET", "/api/v1/watchlist", Some(&session), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().expect("items").len(), 0);
}

#[tokio::test]
async fn watch_is_idempotent() {
    let (router, _storage, _user_id, session) = fresh_app().await;
    let page_id = create_page(router.clone(), &session, "alpha").await;

    for _ in 0..3 {
        let (status, body) = json_request(
            router.clone(),
            "POST",
            "/api/v1/watchlist",
            Some(&session),
            Some(json!({ "page_id": page_id })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
    }

    let (_status, body) =
        json_request(router, "GET", "/api/v1/watchlist", Some(&session), None).await;
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "duplicate watch should leave exactly one row");
}

#[tokio::test]
async fn watchlist_requires_session() {
    let (router, _storage, _user_id, _session) = fresh_app().await;
    let (status, _, _) = request(router.clone(), "GET", "/api/v1/watchlist", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = request(
        router.clone(),
        "POST",
        "/api/v1/watchlist",
        None,
        Some(json!({ "page_id": "00000000-0000-0000-0000-000000000001" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = request(
        router,
        "DELETE",
        "/api/v1/watchlist/00000000-0000-0000-0000-000000000001",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn add_to_watchlist_404s_unknown_page() {
    let (router, _storage, _user_id, session) = fresh_app().await;
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/watchlist",
        Some(&session),
        Some(json!({ "page_id": "00000000-0000-0000-0000-000000000099" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body: {body}");
    assert_eq!(body["code"], "not_found");
}

#[tokio::test]
async fn watch_and_unwatch_write_audit_rows() {
    let (router, storage, user_id, session) = fresh_app().await;
    let page_id_str = create_page(router.clone(), &session, "alpha").await;

    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/watchlist",
        Some(&session),
        Some(json!({ "page_id": page_id_str })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _, _) = request(
        router,
        "DELETE",
        &format!("/api/v1/watchlist/{page_id_str}"),
        Some(&session),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let entries = storage
        .audit_log()
        .list(AuditLogFilter::default(), None, 50)
        .await
        .expect("audit list");
    let actions: Vec<&str> = entries
        .items
        .iter()
        .map(|e| e.action.as_str())
        .filter(|a| a.starts_with("watchlist."))
        .collect();
    assert!(
        actions.contains(&"watchlist.add"),
        "actions: {actions:?}"
    );
    assert!(
        actions.contains(&"watchlist.remove"),
        "actions: {actions:?}"
    );
    // Each row's actor matches the test session.
    for entry in entries.items.iter().filter(|e| e.action.starts_with("watchlist.")) {
        assert_eq!(entry.actor_id, user_id);
        assert_eq!(entry.actor_username, "tester");
    }
}

// ─── Atom feeds ───────────────────────────────────────────────────────────

#[tokio::test]
async fn recent_changes_atom_validates_as_xml() {
    let (router, _storage, _user_id, session) = fresh_app().await;
    create_page(router.clone(), &session, "home").await;

    let (status, headers, bytes) =
        request(router, "GET", "/api/v1/recent-changes.atom", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/atom+xml; charset=utf-8")
    );

    let body = String::from_utf8(bytes).expect("utf8");
    let document = roxmltree::Document::parse(&body).expect("well-formed XML");
    let feed = document.root_element();
    assert_eq!(feed.tag_name().name(), "feed");
    assert_eq!(
        feed.tag_name().namespace(),
        Some("http://www.w3.org/2005/Atom")
    );

    let entry = feed
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "entry")
        .expect("at least one entry");
    let title = entry
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "title")
        .and_then(|n| n.text())
        .expect("title");
    assert_eq!(title, "Main:home");
}

#[tokio::test]
async fn namespace_atom_feed_filters_to_namespace_only() {
    let (router, storage, _user_id, session) = fresh_app().await;
    // Add a second namespace so the filter has something to exclude.
    storage
        .namespaces()
        .create(&Namespace {
            id: NamespaceId::new(),
            slug: NamespaceSlug::new("Help").expect("slug"),
            display_name: "Help".into(),
        })
        .await
        .expect("seed Help");

    create_page(router.clone(), &session, "alpha").await;
    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(&session),
        Some(json!({
            "namespace_slug": "Help",
            "slug": "topic",
            "title": "Topic",
            "content": "# Topic",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let (status, _headers, bytes) = request(
        router,
        "GET",
        "/api/v1/recent-changes/Help/atom",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(bytes).expect("utf8");
    let doc = roxmltree::Document::parse(&body).expect("XML");
    let feed = doc.root_element();
    let entries: Vec<_> = feed
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "entry")
        .collect();
    assert_eq!(entries.len(), 1, "expected only the Help entry");
    let title = entries[0]
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "title")
        .and_then(|n| n.text())
        .expect("title");
    assert_eq!(title, "Help:topic");
}

#[tokio::test]
async fn namespace_atom_404s_for_unknown_namespace() {
    let (router, _storage, _user_id, _session) = fresh_app().await;
    let (status, _, _) = request(
        router,
        "GET",
        "/api/v1/recent-changes/Nowhere/atom",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn protected_pages_are_omitted_from_public_feed() {
    let (router, storage, _user_id, session) = fresh_app().await;
    // Create a page, then bump its protection_level to `Protected` directly
    // via the storage layer. We're testing the feed-side filter, not the
    // edit-side protection flow.
    create_page(router.clone(), &session, "open").await;
    let page_id_str = create_page(router.clone(), &session, "locked").await;
    let page_uuid: uuid::Uuid = page_id_str.parse().expect("uuid");
    let mut page = storage
        .pages()
        .get_by_id(thewiki_core::PageId::from_uuid(page_uuid))
        .await
        .expect("get page");
    page.protection_level = ProtectionLevel::Protected;
    storage.pages().update(&page).await.expect("update page");

    let (status, _, bytes) =
        request(router, "GET", "/api/v1/recent-changes.atom", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let body = String::from_utf8(bytes).expect("utf8");
    assert!(body.contains("Main:open"), "open page should appear");
    assert!(
        !body.contains("Main:locked"),
        "protected page should be filtered out: {body}"
    );
}

#[tokio::test]
async fn watchlist_atom_requires_session() {
    let (router, _storage, _user_id, _session) = fresh_app().await;
    let (status, _, _) = request(router, "GET", "/api/v1/watchlist.atom", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn watchlist_atom_lists_watched_pages_for_session() {
    let (router, _storage, _user_id, session) = fresh_app().await;
    let page_id = create_page(router.clone(), &session, "watched-one").await;
    // Watch it.
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/watchlist",
        Some(&session),
        Some(json!({ "page_id": page_id })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, headers, bytes) =
        request(router, "GET", "/api/v1/watchlist.atom", Some(&session), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/atom+xml; charset=utf-8")
    );

    let body = String::from_utf8(bytes).expect("utf8");
    let doc = roxmltree::Document::parse(&body).expect("XML");
    let feed = doc.root_element();
    let entries: Vec<_> = feed
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "entry")
        .collect();
    assert_eq!(entries.len(), 1, "one watched entry");
    let title = entries[0]
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "title")
        .and_then(|n| n.text())
        .expect("title");
    assert_eq!(title, "Main:watched-one");
}
