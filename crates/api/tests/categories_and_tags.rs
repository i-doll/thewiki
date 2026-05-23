//! Integration tests for the categories + tags endpoints (#29).
//!
//! Each test boots a fresh in-memory SQLite, applies migrations, seeds the
//! default namespace plus a test user, then drives the router via
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
        is_talk: false,
        paired_namespace_id: None,
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

#[tokio::test]
async fn create_category_round_trips() {
    let (router, user_id) = fresh_app().await;
    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/categories",
        Some(user_id),
        Some(json!({"slug": "history", "display_name": "History"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body["slug"], "history");
    assert_eq!(body["display_name"], "History");
    assert!(body["id"].is_string());
    assert!(body["parent_id"].is_null());

    let (status, listing) = json_request(router, "GET", "/api/v1/categories", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {listing}");
    let items = listing["items"].as_array().expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["slug"], "history");
}

#[tokio::test]
async fn assign_categories_on_create() {
    let (router, user_id) = fresh_app().await;
    let (_, history) = json_request(
        router.clone(),
        "POST",
        "/api/v1/categories",
        Some(user_id),
        Some(json!({"slug": "history", "display_name": "History"})),
    )
    .await;
    let (_, ancient) = json_request(
        router.clone(),
        "POST",
        "/api/v1/categories",
        Some(user_id),
        Some(json!({"slug": "ancient", "display_name": "Ancient"})),
    )
    .await;
    let history_id = history["id"].as_str().unwrap();
    let ancient_id = ancient["id"].as_str().unwrap();

    let (status, page) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "rome",
            "title": "Rome",
            "content": "# Rome",
            "categories": [history_id, ancient_id],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {page}");

    let (status, fetched) = json_request(router, "GET", "/api/v1/pages/rome", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {fetched}");
    let categories = fetched["categories"].as_array().expect("categories");
    assert_eq!(categories.len(), 2);
    let slugs: Vec<&str> = categories
        .iter()
        .map(|c| c["slug"].as_str().expect("slug"))
        .collect();
    assert!(slugs.contains(&"history"));
    assert!(slugs.contains(&"ancient"));
}

#[tokio::test]
async fn update_replaces_category_set_atomically() {
    let (router, user_id) = fresh_app().await;
    let (_, history) = json_request(
        router.clone(),
        "POST",
        "/api/v1/categories",
        Some(user_id),
        Some(json!({"slug": "history", "display_name": "History"})),
    )
    .await;
    let (_, geography) = json_request(
        router.clone(),
        "POST",
        "/api/v1/categories",
        Some(user_id),
        Some(json!({"slug": "geography", "display_name": "Geography"})),
    )
    .await;
    let history_id = history["id"].as_str().unwrap();
    let geography_id = geography["id"].as_str().unwrap();

    json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "rome",
            "title": "Rome",
            "content": "# Rome",
            "categories": [history_id],
        })),
    )
    .await;

    // Update — keep only `geography`, the old `history` assignment is gone.
    let (status, updated) = json_request(
        router.clone(),
        "PUT",
        "/api/v1/pages/rome",
        Some(user_id),
        Some(json!({
            "content": "# Rome v2",
            "categories": [geography_id],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {updated}");

    let (_, fetched) = json_request(router, "GET", "/api/v1/pages/rome", None, None).await;
    let categories = fetched["categories"].as_array().expect("categories");
    assert_eq!(categories.len(), 1, "categories: {fetched}");
    assert_eq!(categories[0]["slug"], "geography");
}

#[tokio::test]
async fn list_pages_in_category() {
    let (router, user_id) = fresh_app().await;
    let (_, history) = json_request(
        router.clone(),
        "POST",
        "/api/v1/categories",
        Some(user_id),
        Some(json!({"slug": "history", "display_name": "History"})),
    )
    .await;
    let history_id = history["id"].as_str().unwrap().to_string();

    for slug in ["rome", "athens"] {
        json_request(
            router.clone(),
            "POST",
            "/api/v1/pages",
            Some(user_id),
            Some(json!({
                "namespace_slug": "Main",
                "slug": slug,
                "title": slug,
                "content": "body",
                "categories": [history_id.as_str()],
            })),
        )
        .await;
    }

    let (status, detail) =
        json_request(router, "GET", "/api/v1/categories/history", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {detail}");
    let items = detail["items"].as_array().expect("items");
    assert_eq!(items.len(), 2, "items: {detail}");
    let slugs: Vec<&str> = items
        .iter()
        .map(|p| p["slug"].as_str().expect("slug"))
        .collect();
    assert!(slugs.contains(&"rome"));
    assert!(slugs.contains(&"athens"));
}

#[tokio::test]
async fn assign_tags_and_round_trip() {
    let (router, user_id) = fresh_app().await;
    let (status, _) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "rome",
            "title": "Rome",
            "content": "# Rome",
            "tags": ["history", "Geography", "WAR-101"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, fetched) = json_request(router, "GET", "/api/v1/pages/rome", None, None).await;
    let tags = fetched["tags"]
        .as_array()
        .expect("tags array")
        .iter()
        .map(|t| t.as_str().expect("tag string").to_owned())
        .collect::<Vec<_>>();
    // Lowercased and sorted ascending by storage layer.
    assert_eq!(tags, vec!["geography", "history", "war-101"]);
}

#[tokio::test]
async fn list_pages_with_specific_tag() {
    let (router, user_id) = fresh_app().await;
    json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "rome",
            "title": "Rome",
            "content": "body",
            "tags": ["history"],
        })),
    )
    .await;
    json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "athens",
            "title": "Athens",
            "content": "body",
            "tags": ["history"],
        })),
    )
    .await;
    json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "isolated",
            "title": "Isolated",
            "content": "body",
            "tags": ["other"],
        })),
    )
    .await;

    let (status, body) = json_request(router, "GET", "/api/v1/tags/history", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"].as_array().expect("items");
    assert_eq!(items.len(), 2, "body: {body}");
    let slugs: Vec<&str> = items
        .iter()
        .map(|p| p["slug"].as_str().expect("slug"))
        .collect();
    assert!(slugs.contains(&"rome"));
    assert!(slugs.contains(&"athens"));
}

#[tokio::test]
async fn tag_autocomplete_by_prefix() {
    let (router, user_id) = fresh_app().await;
    json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "rome",
            "title": "Rome",
            "content": "body",
            "tags": ["history", "ancient-history", "geography"],
        })),
    )
    .await;

    let (status, body) = json_request(router, "GET", "/api/v1/tags?prefix=his", None, None).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let items = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|t| t.as_str().expect("tag").to_owned())
        .collect::<Vec<_>>();
    assert_eq!(items, vec!["history"]);
}

#[tokio::test]
async fn tag_validation_rejects_bad_values() {
    let (router, user_id) = fresh_app().await;
    // Empty string is rejected.
    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "bad",
            "title": "Bad",
            "content": ".",
            "tags": [""],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    // Disallowed character.
    let (status, body) = json_request(
        router.clone(),
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "bad2",
            "title": "Bad2",
            "content": ".",
            "tags": ["with space"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

    // Oversize tag (33 chars).
    let oversize = "a".repeat(33);
    let (status, body) = json_request(
        router,
        "POST",
        "/api/v1/pages",
        Some(user_id),
        Some(json!({
            "namespace_slug": "Main",
            "slug": "bad3",
            "title": "Bad3",
            "content": ".",
            "tags": [oversize],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
}
