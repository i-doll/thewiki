//! Integration tests for template transclusion end-to-end (#45).
//!
//! Boots a fresh in-memory SQLite, seeds the `Main` and `Template`
//! namespaces, persists a `Template:` page through the page store, then
//! drives the page-create + page-fetch endpoints to verify that the
//! rendered `content_html` carries the substituted template body.
//!
//! Covers:
//! - happy path (template found, expansion substitutes positional arg)
//! - missing template (renders inline error block)
//! - parser-function rejection (`{{#if:...}}` → "not supported in v1")

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use thewiki_api::AppState;
use thewiki_api::app;
use thewiki_core::{EmailAddress, User, UserId, Username};
use thewiki_storage::repo::{NamespaceRepository, UserRepository};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

/// Boot a fresh router. Seeds both `Main` and `Template` namespaces via the
/// boot path; returns the storage handle so the test can persist a
/// `Template:` page directly.
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

    storage
        .namespaces()
        .get_or_create_default()
        .await
        .expect("seed Main");
    storage
        .namespaces()
        .get_or_create_template_namespace()
        .await
        .expect("seed Template");

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
        .expect("seed user");

    let mut auth_cfg = thewiki_api::config::Config::defaults().auth;
    auth_cfg.anonymous_edits = true;
    let state = AppState::new(storage.clone(), auth_cfg);
    let router = app::build_with_state(state);
    (router, user.id)
}

/// Persist a `Template:Name` page through the namespace-aware create route.
/// Goes through the API so the test exercises the same publish path
/// operators hit.
async fn seed_template(router: &Router, name: &str, body: &str, author: UserId) {
    let (status, resp) = json_request(
        router.clone(),
        "POST",
        "/api/v1/wiki/Template",
        Some(author),
        Some(json!({
            "slug": name,
            "title": name,
            "content": body,
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "seeding template {name}: {resp}"
    );
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
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| panic!("not JSON: {:?}", &bytes))
    };
    (status, json)
}

#[tokio::test]
async fn template_expands_when_referenced_from_a_page() {
    let (router, user_id) = fresh_app().await;

    seed_template(&router, "Greeting", "Hello, {{{1}}}!", user_id).await;

    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "{{Greeting|Aida}}"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let html = body["content_html"].as_str().expect("content_html string");
    assert!(html.contains("Hello, Aida!"), "html = {html}");
    // No template-error span on the happy path.
    assert!(!html.contains("template-error"), "html = {html}");

    // Re-fetch via GET to make sure the render is stable.
    let (status, body) = json_request(router, "GET", "/api/v1/pages/home", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let html = body["content_html"].as_str().expect("content_html string");
    assert!(html.contains("Hello, Aida!"), "html = {html}");
}

#[tokio::test]
async fn missing_template_surfaces_inline_error() {
    let (router, user_id) = fresh_app().await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "Before {{NoSuchTemplate}} after"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let html = body["content_html"].as_str().expect("content_html string");
    assert!(
        html.contains("template-error"),
        "expected inline error, html = {html}"
    );
    assert!(
        html.contains("Template:NoSuchTemplate"),
        "expected 'not found' diagnostic, html = {html}"
    );
    // Surrounding text survived the pre-pass.
    assert!(html.contains("Before"), "html = {html}");
    assert!(html.contains("after"), "html = {html}");
}

#[tokio::test]
async fn parser_function_emits_unsupported_error_inline() {
    let (router, user_id) = fresh_app().await;

    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "home",
            "title": "Home",
            "content": "Before {{#if:cond|yes|no}} after"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let html = body["content_html"].as_str().expect("content_html string");
    assert!(html.contains("template-error"), "html = {html}");
    // Single-quote is HTML-escaped (`&#39;`) inside the diagnostic span,
    // but the human-readable phrase still appears as a substring.
    assert!(
        html.contains("parser function") && html.contains("#if"),
        "expected '#if not supported' diagnostic, html = {html}"
    );
}
