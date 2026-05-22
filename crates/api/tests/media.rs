//! Integration tests for the media upload endpoints (#32).
//!
//! Each test boots a fresh in-memory SQLite, applies migrations, seeds a
//! user + session, and drives the router via `tower::ServiceExt::oneshot`.
//! The DB-blob backend is selected so we don't need an S3 emulator on CI.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::Value;
use thewiki_api::AppState;
use thewiki_api::app;
use thewiki_api::auth::password::Argon2Hasher;
use thewiki_api::auth::state::AuthState;
use thewiki_api::config::{Argon2Config, Config, MediaConfig, StorageBackend};
use thewiki_api::media::build_media_backend;
use thewiki_core::{User, UserId, Username};
use thewiki_storage::repo::{SessionRepository, UserRepository};
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use time::OffsetDateTime;
use tower::ServiceExt;

/// CSRF token used by every authenticated request in this suite — passed
/// as both the cookie value and the matching `X-CSRF-Token` header so the
/// double-submit middleware lets the request through.
const TEST_CSRF: &str = "test-csrf-token-fixed-value-32b";

/// Multipart boundary used by every upload in this suite. Hard-coded so
/// the helpers below can build the body byte-by-byte without hauling in a
/// multipart-writer dependency.
const BOUNDARY: &str = "------thewikitestboundary";

/// Tiny 1x1 transparent PNG (67 bytes). Pre-computed here so tests don't
/// need a PNG encoder. The bytes were generated once via `convert -size 1x1
/// xc:none png:` and verified by checking the IHDR / IEND markers.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// Argon2 parameters at the OWASP floor for fast test startup.
fn test_argon2() -> Argon2Config {
    Argon2Config {
        memory_kib: 19_456,
        iterations: 2,
        parallelism: 1,
    }
}

fn disabled_rate_limit() -> thewiki_api::config::RateLimitConfig {
    let mut cfg = Config::defaults().rate_limit;
    cfg.enabled = false;
    cfg
}

/// Build a router with the media pipeline wired against the in-DB backend.
async fn fresh_app() -> (Router, UserId, SqliteStorage) {
    fresh_app_with_media_config(MediaConfig::default()).await
}

async fn fresh_app_with_media_config(media_config: MediaConfig) -> (Router, UserId, SqliteStorage) {
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

    let user = User {
        id: UserId::new(),
        username: Username::new("uploader").expect("valid username"),
        email: None,
        display_name: Some("Uploader".into()),
        created_at: OffsetDateTime::now_utc() - time::Duration::days(30),
        last_login_at: None,
    };
    storage
        .users()
        .create(&user, None)
        .await
        .expect("seed user");

    let hasher = Arc::new(Argon2Hasher::new(test_argon2()).expect("hasher"));
    let auth_cfg = Config::defaults().auth;
    let auth_state = AuthState::new(
        storage.clone(),
        hasher,
        Duration::from_secs(60 * 60),
        false,
        auth_cfg.clone(),
    );

    let mut state = AppState::new(storage.clone(), auth_cfg).with_auth_state(auth_state.clone());
    let backend = build_media_backend(&StorageBackend::Db, Arc::clone(&state.storage))
        .expect("DB backend init");
    state = state.with_media(media_config, backend);

    let router = app::build_full(state, auth_state, false, disabled_rate_limit());
    (router, user.id, storage)
}

/// Seed a session for `user_id` and return the cookie value.
async fn seed_session(storage: &SqliteStorage, user_id: UserId) -> String {
    let session = storage
        .sessions()
        .create(user_id, Duration::from_secs(60 * 60), None, None)
        .await
        .expect("seed session");
    session.id.into_uuid().to_string()
}

/// Build the `multipart/form-data` body and Content-Type header for a single
/// `file` field with `bytes` and an inline `Content-Type`. Hand-rolled
/// rather than pulled from a multipart writer crate to keep test deps thin
/// and the wire shape obvious.
fn multipart_file(
    field_name: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    let preamble = format!(
        "--{BOUNDARY}\r\n\
         Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{filename}\"\r\n\
         Content-Type: {content_type}\r\n\
         \r\n"
    );
    body.extend_from_slice(preamble.as_bytes());
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    let header_value = format!("multipart/form-data; boundary={BOUNDARY}");
    (header_value, body)
}

async fn upload(
    router: &Router,
    session: &str,
    field: &str,
    filename: &str,
    content_type: &str,
    bytes: &[u8],
) -> (StatusCode, Vec<u8>) {
    let (ct_header, body) = multipart_file(field, filename, content_type, bytes);
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/media")
        .header(header::CONTENT_TYPE, ct_header)
        .header(
            header::COOKIE,
            format!("thewiki_session={session}; thewiki_csrf={TEST_CSRF}"),
        )
        .header("x-csrf-token", TEST_CSRF)
        .body(Body::from(body))
        .expect("build request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router responded");
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .expect("read body")
        .to_bytes()
        .to_vec();
    (status, body)
}

fn parse_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| panic!("response wasn't JSON: {:?}", String::from_utf8_lossy(bytes)))
}

#[tokio::test]
async fn upload_png_returns_media_view() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;

    let (status, body) = upload(&router, &session, "file", "tiny.png", "image/png", TINY_PNG).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "body: {:?}",
        String::from_utf8_lossy(&body)
    );

    let json = parse_json(&body);
    assert!(json["id"].is_string(), "missing id: {json}");
    assert_eq!(json["content_type"], "image/png");
    assert_eq!(json["byte_size"], TINY_PNG.len() as i64);
    assert_eq!(json["original_filename"], "tiny.png");
    let url = json["url"].as_str().expect("url");
    let id = json["id"].as_str().expect("id");
    assert_eq!(url, format!("/api/v1/media/{id}"));
    // SHA-256 of the canonical bytes — hex string is 64 chars.
    let hex = json["content_hash_hex"].as_str().expect("hash");
    assert_eq!(hex.len(), 64);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn get_returns_stored_bytes_with_content_type() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;

    let (_, body) = upload(&router, &session, "file", "tiny.png", "image/png", TINY_PNG).await;
    let json = parse_json(&body);
    let url = json["url"].as_str().expect("url").to_owned();

    let get = Request::builder()
        .method("GET")
        .uri(&url)
        .body(Body::empty())
        .expect("build get");
    let response = router.clone().oneshot(get).await.expect("get responded");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok()),
        Some("image/png")
    );
    let cache = response
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|h| h.to_str().ok())
        .expect("cache-control");
    assert!(
        cache.contains("immutable"),
        "expected immutable cache; got {cache}"
    );
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("get body")
        .to_bytes();
    assert_eq!(&bytes[..], TINY_PNG);
}

#[tokio::test]
async fn duplicate_upload_returns_existing_id() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;

    let (s1, b1) = upload(&router, &session, "file", "tiny.png", "image/png", TINY_PNG).await;
    assert_eq!(s1, StatusCode::OK);
    let id1 = parse_json(&b1)["id"].as_str().expect("id1").to_owned();

    // Second upload of the same content — different filename to prove the
    // dedup key is the hash and not the name.
    let (s2, b2) = upload(
        &router,
        &session,
        "file",
        "renamed.png",
        "image/png",
        TINY_PNG,
    )
    .await;
    assert_eq!(s2, StatusCode::OK);
    let id2 = parse_json(&b2)["id"].as_str().expect("id2").to_owned();
    assert_eq!(id1, id2, "expected same id on dedup");
}

#[tokio::test]
async fn oversize_upload_returns_413() {
    let small_limit = MediaConfig {
        max_upload_bytes: 32,
        ..MediaConfig::default()
    };
    let (router, user_id, storage) = fresh_app_with_media_config(small_limit).await;
    let session = seed_session(&storage, user_id).await;

    let too_big = vec![0u8; 64];
    let (status, body) = upload(&router, &session, "file", "big.png", "image/png", &too_big).await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "body: {:?}",
        String::from_utf8_lossy(&body)
    );
    let json = parse_json(&body);
    assert_eq!(json["code"], "payload_too_large");
}

#[tokio::test]
async fn forbidden_content_type_returns_415() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;

    let (status, body) = upload(
        &router,
        &session,
        "file",
        "evil.exe",
        "application/x-msdownload",
        b"MZ\x90\x00",
    )
    .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let json = parse_json(&body);
    assert_eq!(json["code"], "unsupported_media_type");
}

#[tokio::test]
async fn svg_upload_strips_script_tag() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;

    let dangerous_svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\">\
        <script>alert(1)</script>\
        <rect width=\"10\" height=\"10\" onclick=\"alert(2)\" fill=\"red\"/>\
        </svg>";
    let (status, body) = upload(
        &router,
        &session,
        "file",
        "evil.svg",
        "image/svg+xml",
        dangerous_svg,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "body: {:?}",
        String::from_utf8_lossy(&body)
    );
    let json = parse_json(&body);
    let url = json["url"].as_str().expect("url").to_owned();

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(&url)
                .body(Body::empty())
                .expect("build get"),
        )
        .await
        .expect("router responded");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    let text = std::str::from_utf8(&bytes).expect("svg utf-8");
    assert!(
        !text.contains("<script"),
        "stored SVG should not contain <script>: {text}"
    );
    assert!(
        !text.to_lowercase().contains("onclick"),
        "stored SVG should not contain onclick handlers: {text}"
    );
}

#[tokio::test]
async fn delete_without_session_returns_401() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;

    // Upload first so we have a real id to target — otherwise we'd be
    // testing the 401-before-route-match path through a 404, and the
    // ordering of `RequireAuth` vs `Path` extraction isn't actually
    // specified.
    let (_, body) = upload(&router, &session, "file", "tiny.png", "image/png", TINY_PNG).await;
    let id = parse_json(&body)["id"].as_str().expect("id").to_owned();

    // Unauthenticated DELETE — no cookie, no header.
    let request = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/media/{id}"))
        .body(Body::empty())
        .expect("build delete");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("router responded");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_then_get_returns_404() {
    let (router, user_id, storage) = fresh_app().await;
    let session = seed_session(&storage, user_id).await;

    let (_, body) = upload(&router, &session, "file", "tiny.png", "image/png", TINY_PNG).await;
    let id = parse_json(&body)["id"].as_str().expect("id").to_owned();

    let del = Request::builder()
        .method("DELETE")
        .uri(format!("/api/v1/media/{id}"))
        .header(
            header::COOKIE,
            format!("thewiki_session={session}; thewiki_csrf={TEST_CSRF}"),
        )
        .header("x-csrf-token", TEST_CSRF)
        .body(Body::empty())
        .expect("build delete");
    let response = router.clone().oneshot(del).await.expect("router responded");
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let get = Request::builder()
        .method("GET")
        .uri(format!("/api/v1/media/{id}"))
        .body(Body::empty())
        .expect("build get");
    let response = router.clone().oneshot(get).await.expect("router responded");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
