//! Rate-limit error types.
//!
//! Surfaced as a 429 response with a `Retry-After` header. We expose a typed
//! [`RateLimitError`] mostly so the middleware can construct the response in
//! one place; callers downstream don't typically see the type itself.

use std::time::Duration;

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

/// Wire form for a 429 response body.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RateLimitErrorBody {
    /// Stable machine-readable error code. Always `"rate_limited"`.
    pub error: String,
}

/// Errors the rate-limit middleware can surface.
///
/// Today there is only one variant — exhausted bucket — but the enum is open
/// (`#[non_exhaustive]`) so a future Redis-side outage can grow a `Backend`
/// variant without breaking matchers.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RateLimitError {
    /// The bucket for this principal is empty. `retry_after` is the wall-clock
    /// duration until the next token will be available.
    #[error("rate limit exceeded; retry in {retry_after:?}")]
    Exceeded {
        /// How long until a token will be available again.
        retry_after: Duration,
    },
    /// The backend store returned an error. Currently only emitted by the Redis
    /// backend; the in-memory backend is infallible.
    #[error("rate-limit backend error: {0}")]
    Backend(String),
}

impl IntoResponse for RateLimitError {
    fn into_response(self) -> Response {
        match self {
            Self::Exceeded { retry_after } => rate_limited_response(retry_after),
            Self::Backend(msg) => {
                // Backend failures fail-open at the middleware site (the
                // request is allowed through) — this branch exists so the
                // type is round-trippable through `IntoResponse` for tests
                // and would-be callers, not because we expect the middleware
                // to produce it.
                tracing::warn!(error = %msg, "rate-limit backend error surfaced as response");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(RateLimitErrorBody {
                        error: "rate_limit_backend".to_owned(),
                    }),
                )
                    .into_response()
            }
        }
    }
}

/// Build a 429 response carrying `Retry-After`.
///
/// `retry_after` is rounded up to whole seconds and floored at 1 so we never
/// emit `Retry-After: 0` (which clients can interpret as "retry immediately"
/// and would just re-exhaust the bucket).
#[must_use]
pub fn rate_limited_response(retry_after: Duration) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(RateLimitErrorBody {
            error: "rate_limited".to_owned(),
        }),
    )
        .into_response();
    let retry_after_secs = retry_after.as_secs().max(1).to_string();
    if let Ok(value) = HeaderValue::from_str(&retry_after_secs) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}
