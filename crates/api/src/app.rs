//! Axum application wiring: routes and middleware stack.
//!
//! The router exposed by [`build`] is the single source of truth used by both
//! the production binary and integration tests. Keep handler logic out of this
//! module — it should read as a table of routes plus the middleware layering.

use axum::{
    Router,
    http::{HeaderName, HeaderValue, Request},
    response::IntoResponse,
    routing::get,
};
use tower::ServiceBuilder;
use tower_http::{
    catch_panic::CatchPanicLayer,
    request_id::{MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer},
    trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer},
};
use tracing::Level;
use uuid::Uuid;

/// HTTP header used to carry the per-request correlation ID.
const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Build the application router with all middleware applied.
///
/// Layers, outermost first (i.e. first to see the request, last to see the
/// response):
///
/// 1. [`CatchPanicLayer`] — turns panics in downstream services into `500`s so
///    the worker thread isn't torn down.
/// 2. [`SetRequestIdLayer`] — trusts an incoming `x-request-id` if present,
///    otherwise generates a UUIDv7.
/// 3. [`TraceLayer`] — emits a span per request with method/uri/status/latency
///    and the request ID (which is on the request extensions by this point).
/// 4. [`PropagateRequestIdLayer`] — copies the request ID onto the outgoing
///    response so clients can correlate.
pub fn build() -> Router {
    let middleware = ServiceBuilder::new()
        .layer(CatchPanicLayer::new())
        .layer(SetRequestIdLayer::new(
            REQUEST_ID_HEADER.clone(),
            MakeUuidV7RequestId,
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(
                    DefaultMakeSpan::new()
                        .level(Level::INFO)
                        .include_headers(false),
                )
                .on_response(
                    DefaultOnResponse::new()
                        .level(Level::INFO)
                        .latency_unit(tower_http::LatencyUnit::Millis),
                ),
        )
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER.clone()));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .layer(middleware)
}

/// Liveness probe. Returns 200 as soon as the process is accepting requests.
async fn healthz() -> impl IntoResponse {
    "ok"
}

/// Readiness probe.
///
/// TODO(#7): once #4 lands the storage layer, this should check that the DB
/// pool is live and that all migrations have been applied. For the skeleton
/// PR we always return 200 so the route shape is stable.
async fn readyz() -> impl IntoResponse {
    "ok"
}

/// `MakeRequestId` impl that mints a UUIDv7 per request.
///
/// UUIDv7 sorts lexicographically by creation time, which makes logs grouped
/// by request ID easy to follow.
#[derive(Clone, Copy, Default)]
struct MakeUuidV7RequestId;

impl MakeRequestId for MakeUuidV7RequestId {
    fn make_request_id<B>(&mut self, _request: &Request<B>) -> Option<RequestId> {
        let id = Uuid::now_v7();
        // UUID hyphenated form is always ASCII; `HeaderValue::from_str` cannot fail.
        let value = HeaderValue::from_str(&id.to_string()).ok()?;
        Some(RequestId::new(value))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok() {
        let app = build();
        let response = app
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
            .expect("read body")
            .to_bytes();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn readyz_returns_ok() {
        let app = build();
        let response = app
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
}
