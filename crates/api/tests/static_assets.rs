//! Integration tests for the embedded SPA fallback (#16).
//!
//! `rust-embed` runs in debug mode during `cargo test`, so it reads
//! `web/dist/` from disk on every `get`. The crate's `build.rs` already
//! ensures `web/dist/index.html` exists (placeholder or real bundle); this
//! test additionally drops `web/dist/assets/foo.js` so we can assert against
//! a known hashed asset. Writes are idempotent — a real `pnpm build` run
//! won't be clobbered because we only touch the test fixture file.
//!
//! The tests boot the full router (auth + pages + fallback) and exercise:
//!
//! - `GET /` returns 200 + `text/html` from `index.html`.
//! - `GET /assets/foo.js` returns 200 + `application/javascript` +
//!   immutable cache-control + an ETag.
//! - `GET /some/spa/route` (no real file) falls back to `index.html`.
//! - `GET /api/v1/pages/nonexistent` returns 404 — the SPA fallback never
//!   eats an API miss.
//! - With `serve_frontend = false`, `GET /` returns 404 (Vite-style dev).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Once;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use thewiki_api::AppState;
use thewiki_api::app;
use thewiki_api::auth::password::Argon2Hasher;
use thewiki_api::auth::state::AuthState;
use thewiki_api::config::Argon2Config;
use thewiki_core::{Namespace, NamespaceId, NamespaceSlug};
use thewiki_storage::repo::NamespaceRepository;
use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
use tower::ServiceExt;

/// Test-only Vite asset body. Distinguishing comment + bytes so a real
/// `pnpm build` overwrite would be visible in test failures.
const TEST_ASSET_JS: &str = "// test fixture for /assets/foo.js\nexport const x = 1;\n";

static SETUP: Once = Once::new();

/// One-shot fixture writer. Ensures `web/dist/assets/foo.js` exists for the
/// test run; never overwrites it (so a real Vite bundle's asset file with
/// the same name — vanishingly unlikely — wouldn't be clobbered). Also
/// re-asserts that the build-script-managed `index.html` is present.
fn ensure_dist_fixtures() {
    SETUP.call_once(|| {
        let dist = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("web")
            .join("dist");

        let assets = dist.join("assets");
        std::fs::create_dir_all(&assets).expect("create web/dist/assets");

        let fixture = assets.join("foo.js");
        if !fixture.exists() {
            std::fs::write(&fixture, TEST_ASSET_JS).expect("write web/dist/assets/foo.js");
        }

        // build.rs writes a placeholder if a real bundle is missing. Either
        // way, this file must exist by the time tests run.
        assert!(
            dist.join("index.html").exists(),
            "web/dist/index.html should exist (build.rs writes a placeholder); \
             did the build script fail?"
        );
    });
}

/// OWASP-floor Argon2 parameters so tests stay fast.
fn test_argon2() -> Argon2Config {
    Argon2Config {
        memory_kib: 19_456,
        iterations: 2,
        parallelism: 1,
    }
}

/// Spin up the full router with the SPA fallback enabled or disabled per
/// the `serve_frontend` flag.
async fn fresh_app_with_frontend(serve_frontend: bool) -> Router {
    ensure_dist_fixtures();

    let storage = SqliteStorage::new(
        "sqlite::memory:",
        SqliteOptions {
            max_connections: 1,
            acquire_timeout: Duration::from_secs(5),
            foreign_keys: true,
        },
    )
    .await
    .expect("open in-memory sqlite");

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

    let hasher = Arc::new(Argon2Hasher::new(test_argon2()).expect("hasher"));
    let auth_state = AuthState::new(storage.clone(), hasher, Duration::from_secs(60 * 60), false);
    let app_state = AppState::new(storage);

    app::build_full(app_state, auth_state, serve_frontend)
}

async fn get(router: &Router, uri: &str) -> axum::http::Response<Body> {
    router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router responded")
}

#[tokio::test]
async fn index_html_served_at_root() {
    let router = fresh_app_with_frontend(true).await;

    let response = get(&router, "/").await;
    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("content-type")
        .to_str()
        .expect("ascii");
    assert!(
        content_type.starts_with("text/html"),
        "expected text/html, got {content_type}"
    );

    let cache_control = response
        .headers()
        .get(header::CACHE_CONTROL)
        .expect("cache-control")
        .to_str()
        .expect("ascii");
    assert_eq!(cache_control, "no-cache");

    assert!(
        response.headers().get(header::ETAG).is_some(),
        "expected ETag header on index.html"
    );

    let body = response
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    assert!(!body.is_empty(), "index.html body should be non-empty");
}

#[tokio::test]
async fn hashed_asset_gets_immutable_cache() {
    let router = fresh_app_with_frontend(true).await;

    let response = get(&router, "/assets/foo.js").await;
    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("content-type")
        .to_str()
        .expect("ascii");
    // `mime_guess` maps `.js` to `application/javascript` (or `text/javascript`
    // on newer versions). Accept either to stay forward-compatible.
    assert!(
        content_type.contains("javascript"),
        "expected a javascript MIME, got {content_type}"
    );

    let cache_control = response
        .headers()
        .get(header::CACHE_CONTROL)
        .expect("cache-control")
        .to_str()
        .expect("ascii");
    assert_eq!(cache_control, "public, max-age=31536000, immutable");

    assert!(
        response.headers().get(header::ETAG).is_some(),
        "ETag missing"
    );
}

#[tokio::test]
async fn spa_history_route_falls_back_to_index() {
    let router = fresh_app_with_frontend(true).await;

    let response = get(&router, "/wiki/Some-Page").await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "SPA history route should return index.html with 200"
    );
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("content-type")
        .to_str()
        .expect("ascii");
    assert!(content_type.starts_with("text/html"));

    let body = response
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    assert!(!body.is_empty(), "SPA fallback body should be non-empty");
}

#[tokio::test]
async fn api_miss_returns_404_even_with_spa_fallback() {
    let router = fresh_app_with_frontend(true).await;

    let response = get(&router, "/api/v1/pages/totally-not-a-real-route").await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "/api/* misses must 404, not serve the SPA shell"
    );
}

#[tokio::test]
async fn serve_frontend_false_returns_404_at_root() {
    let router = fresh_app_with_frontend(false).await;

    let response = get(&router, "/").await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "with serve_frontend=false, root should 404 so Vite handles it"
    );
}
