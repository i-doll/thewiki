//! Integration tests for the health endpoints and request-ID propagation.
//!
//! Tests use `tower::ServiceExt::oneshot` against the in-process `Router`
//! returned by [`thewiki_api::app::build`]. No real listener is bound.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use thewiki_api::app;
use tower::ServiceExt;

#[tokio::test]
async fn healthz_returns_200_with_ok_body() {
    let response = app::build()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .expect("build healthz request"),
        )
        .await
        .expect("router responded");

    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    assert_eq!(&body[..], b"ok");
}

#[tokio::test]
async fn readyz_returns_200() {
    let response = app::build()
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .expect("build readyz request"),
        )
        .await
        .expect("router responded");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn request_id_is_generated_when_absent() {
    let response = app::build()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router responded");

    let id = response
        .headers()
        .get("x-request-id")
        .expect("x-request-id header should be set by middleware")
        .to_str()
        .expect("request id should be ascii");

    // UUIDv7 hyphenated form: 8-4-4-4-12 = 36 chars.
    assert_eq!(
        id.len(),
        36,
        "expected a UUID-shaped request id, got {id:?}"
    );
}

#[tokio::test]
async fn request_id_is_trusted_when_supplied() {
    const SUPPLIED: &str = "test-supplied-request-id";

    let response = app::build()
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .header("x-request-id", SUPPLIED)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router responded");

    let id = response
        .headers()
        .get("x-request-id")
        .expect("x-request-id header should be present")
        .to_str()
        .expect("request id should be ascii");
    assert_eq!(id, SUPPLIED);
}
