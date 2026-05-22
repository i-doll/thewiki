//! Integration tests for `GET /api/v1/pages/{slug}/backlinks` and the
//! page-link maintenance pipeline (#30).
//!
//! Each test boots a fresh in-memory SQLite, applies migrations, seeds the
//! default namespace + a test user, then drives the router via
//! `tower::ServiceExt::oneshot`. No TCP listener is bound.

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

    let mut auth_cfg = thewiki_api::config::Config::defaults().auth;
    auth_cfg.anonymous_edits = true;
    let state = AppState::new(storage, auth_cfg);
    let router = app::build_with_state(state);
    (router, user.id)
}

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
        let parsed: Value = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| panic!("response wasn't JSON: {:?}", &bytes));
        (status, parsed)
    }
}

async fn create_page(router: Router, user_id: UserId, slug: &str, title: &str, content: &str) {
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": slug,
            "title": title,
            "content": content,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
}

#[tokio::test]
async fn backlinks_returns_one_entry_when_b_links_to_a() {
    let (router, user_id) = fresh_app().await;

    // Create A first so the namespace exists; the link target is the slug
    // string, not a page-id reference, so order isn't load-bearing for the
    // `page_links` row.
    create_page(router.clone(), user_id, "a", "A", "Page A").await;
    create_page(router.clone(), user_id, "b", "B", "Page B links to [[a]].").await;

    let (status, body) = json_request(router, "GET", "/api/v1/pages/a/backlinks", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "items: {items:?}");
    assert_eq!(items[0]["page_slug"], "b");
    assert_eq!(items[0]["title"], "B");
    assert_eq!(items[0]["namespace_slug"], "Main");
    assert!(items[0]["page_id"].is_string());
    assert!(body["next_cursor"].is_null());
}

#[tokio::test]
async fn backlinks_empty_for_page_with_no_inbound_links() {
    let (router, user_id) = fresh_app().await;
    create_page(router.clone(), user_id, "lonely", "Lonely", "no links here").await;

    let (status, body) =
        json_request(router, "GET", "/api/v1/pages/lonely/backlinks", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items array");
    assert!(items.is_empty(), "items: {items:?}");
}

#[tokio::test]
async fn backlinks_for_missing_target_still_lists_inbound_pages() {
    // Redlinks are first-class — pages can reference a target before the
    // target exists, and the backlinks list must show those references so
    // the editor can create the page knowing who's waiting on it.
    let (router, user_id) = fresh_app().await;
    create_page(
        router.clone(),
        user_id,
        "intro",
        "Intro",
        "See also [[NotYet]] for the deep dive.",
    )
    .await;

    let (status, body) =
        json_request(router, "GET", "/api/v1/pages/NotYet/backlinks", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "items: {items:?}");
    assert_eq!(items[0]["page_slug"], "intro");
}

#[tokio::test]
async fn updating_a_page_replaces_its_outbound_link_set() {
    let (router, user_id) = fresh_app().await;
    create_page(router.clone(), user_id, "target", "Target", "").await;
    create_page(
        router.clone(),
        user_id,
        "source",
        "Source",
        "See [[target]].",
    )
    .await;

    // Sanity: target has one backlink (source).
    let (_, body) = json_request(
        router.clone(),
        "GET",
        "/api/v1/pages/target/backlinks",
        None,
        None,
    )
    .await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1, "{body}");

    // Update source to remove the wikilink.
    let (status, _) = json_request(
        router.clone(),
        "PUT",
        "/api/v1/pages/source",
        Some(user_id),
        Some(json!({
            "content": "now without the link",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Backlinks for `target` must now be empty.
    let (_, body) = json_request(router, "GET", "/api/v1/pages/target/backlinks", None, None).await;
    assert!(
        body["items"].as_array().unwrap().is_empty(),
        "wikilink-removed source should no longer appear: {body}"
    );
}

#[tokio::test]
async fn pipe_display_still_records_the_target_in_backlinks() {
    let (router, user_id) = fresh_app().await;
    create_page(router.clone(), user_id, "core", "Core", "").await;
    create_page(
        router.clone(),
        user_id,
        "essay",
        "Essay",
        "The [[core|fundamental concept]] is important.",
    )
    .await;

    let (status, body) =
        json_request(router, "GET", "/api/v1/pages/core/backlinks", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["page_slug"], "essay");
}

#[tokio::test]
async fn backlinks_paginates_when_more_than_limit_sources() {
    let (router, user_id) = fresh_app().await;
    create_page(router.clone(), user_id, "hub", "Hub", "").await;

    // Seed five pages, each linking to "hub". The limit=2 paging walks
    // them in `(source_page_id ASC)` order — UUIDv7 sorts by creation
    // time, so the order is "oldest source first".
    for i in 0..5 {
        let slug = format!("src-{i}");
        let title = format!("Src {i}");
        create_page(router.clone(), user_id, &slug, &title, "Reference [[hub]].").await;
    }

    let (status, body) = json_request(
        router.clone(),
        "GET",
        "/api/v1/pages/hub/backlinks?limit=2",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "first page: {items:?}");
    let cursor = body["next_cursor"]
        .as_str()
        .expect("expected a next_cursor after first page")
        .to_string();

    // Walk subsequent pages, collecting every slug we see.
    let mut seen: Vec<String> = items
        .iter()
        .map(|i| i["page_slug"].as_str().unwrap().to_string())
        .collect();
    let mut next_cursor = Some(cursor);
    while let Some(c) = next_cursor.take() {
        let uri = format!("/api/v1/pages/hub/backlinks?limit=2&cursor={c}");
        let (status, body) = json_request(router.clone(), "GET", &uri, None, None).await;
        assert_eq!(status, StatusCode::OK);
        let items = body["items"].as_array().unwrap();
        for item in items {
            seen.push(item["page_slug"].as_str().unwrap().to_string());
        }
        if let Some(nc) = body["next_cursor"].as_str() {
            next_cursor = Some(nc.to_string());
        }
    }

    // All five sources visible exactly once, no duplicates.
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 5, "seen: {seen:?}");
    for i in 0..5 {
        assert!(
            seen.contains(&format!("src-{i}")),
            "missing src-{i}: {seen:?}"
        );
    }
}

#[tokio::test]
async fn page_view_includes_rendered_content_html_with_redlink_class() {
    let (router, user_id) = fresh_app().await;

    // Create the "Hub" target so [[hub]] resolves; leave [[notyet]] as a
    // redlink so we can assert the styling propagated through the API.
    create_page(router.clone(), user_id, "hub", "Hub", "").await;
    create_page(
        router.clone(),
        user_id,
        "home",
        "Home",
        "Visit [[hub]] and [[notyet]].",
    )
    .await;

    let (status, body) = json_request(router, "GET", "/api/v1/pages/home", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let html = body["content_html"]
        .as_str()
        .expect("content_html must be present on PageView");

    assert!(
        html.contains("href=\"/wiki/Main/hub\""),
        "rendered HTML should resolve [[hub]] to its wiki path: {html}"
    );
    assert!(
        html.contains("class=\"redlink\""),
        "rendered HTML should mark [[notyet]] as a redlink: {html}"
    );
    assert!(
        html.contains("href=\"/wiki/Main/notyet/edit?new=1\""),
        "redlink should point at the create form: {html}"
    );
    // Raw Markdown still travels alongside.
    assert!(
        body["content"].as_str().unwrap().contains("[[hub]]"),
        "raw Markdown still available: {body}"
    );
}

#[tokio::test]
async fn deleting_a_source_page_drops_its_outbound_links() {
    let (router, user_id) = fresh_app().await;
    create_page(router.clone(), user_id, "hub", "Hub", "").await;
    create_page(
        router.clone(),
        user_id,
        "satellite",
        "Satellite",
        "Talks about [[hub]].",
    )
    .await;

    let (_, body) = json_request(
        router.clone(),
        "GET",
        "/api/v1/pages/hub/backlinks",
        None,
        None,
    )
    .await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);

    // Drop satellite — schema has ON DELETE CASCADE on page_links so the
    // outbound row goes with it.
    let (status, _) = json_request(
        router.clone(),
        "DELETE",
        "/api/v1/pages/satellite",
        Some(user_id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (_, body) = json_request(router, "GET", "/api/v1/pages/hub/backlinks", None, None).await;
    assert!(body["items"].as_array().unwrap().is_empty(), "{body}");
}
