//! HTTP-shaped errors for the API layer.
//!
//! [`ApiError`] is the single error type every handler returns. It implements
//! [`IntoResponse`] so handlers can `?`-bubble straight to the wire. The
//! mapping from [`StorageError`] to HTTP status follows REST conventions:
//!
//! | Source                       | Status | Reason                          |
//! |------------------------------|--------|---------------------------------|
//! | `StorageError::NotFound`     | 404    | row absent                      |
//! | `StorageError::Conflict`     | 409    | uniqueness violation            |
//! | `StorageError::InvalidInput` | 400    | malformed cursor / column data  |
//! | `StorageError::Database`     | 500    | driver / pool failure           |
//! | `StorageError::Migration`    | 500    | should not happen at runtime    |
//!
//! Auth-related variants (`Unauthenticated`, `Forbidden`) carry their own
//! status. The error body is always a small JSON object — the wire form is
//! described by [`ErrorBody`].

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thewiki_storage::StorageError;
use utoipa::ToSchema;

/// The wire form of an API error.
///
/// Kept intentionally small: a stable machine-readable `code` plus a free-form
/// human message. New variants can be added by extending [`ApiError`] without
/// changing this shape.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ErrorBody {
    /// Stable machine-readable code (`not_found`, `conflict`, …).
    pub code: &'static str,
    /// Human-readable description of what went wrong.
    pub message: String,
}

/// Error type returned by every API handler.
///
/// `#[non_exhaustive]` so adding new auth- or rate-limit-shaped variants
/// later is not a breaking change for the (small) set of internal callers
/// that match on this enum.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ApiError {
    /// The requested resource does not exist. Renders as `404 Not Found`.
    #[error("not found")]
    NotFound,

    /// A uniqueness constraint was violated. Renders as `409 Conflict`.
    #[error("conflict: {0}")]
    Conflict(String),

    /// Caller-supplied input failed validation. Renders as `400 Bad Request`.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// The caller could not be authenticated. Renders as `401 Unauthorized`.
    #[error("unauthenticated")]
    Unauthenticated,

    /// The caller is authenticated but lacks permission. Renders as
    /// `403 Forbidden`.
    #[error("forbidden")]
    Forbidden,

    /// The request body exceeded the operator-configured size cap. Renders
    /// as `413 Payload Too Large`. Used by the media upload endpoint when
    /// the field length exceeds `storage.media.max_upload_bytes`.
    #[error("payload too large (limit: {limit} bytes)")]
    PayloadTooLarge {
        /// Configured byte limit at the time of the request.
        limit: u64,
    },

    /// The upload's `Content-Type` was not in the operator allowlist.
    /// Renders as `415 Unsupported Media Type`.
    #[error("unsupported media type: {0}")]
    UnsupportedMediaType(String),

    /// An internal error escaped without a more specific mapping. Renders as
    /// `500 Internal Server Error`. The error chain is logged; the wire form
    /// only carries a generic message so we don't leak internals to callers.
    #[error("internal error: {0}")]
    Internal(String),
}

impl ApiError {
    /// Status code this variant maps to.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::InvalidInput(_) => StatusCode::BAD_REQUEST,
            Self::Unauthenticated => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::PayloadTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedMediaType(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Machine-readable code carried in the response body.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Conflict(_) => "conflict",
            Self::InvalidInput(_) => "invalid_input",
            Self::Unauthenticated => "unauthenticated",
            Self::Forbidden => "forbidden",
            Self::PayloadTooLarge { .. } => "payload_too_large",
            Self::UnsupportedMediaType(_) => "unsupported_media_type",
            Self::Internal(_) => "internal_error",
        }
    }

    /// Build the wire body for this variant.
    #[must_use]
    pub fn body(&self) -> ErrorBody {
        let message = match self {
            Self::NotFound | Self::Unauthenticated | Self::Forbidden => self.to_string(),
            Self::Conflict(msg) | Self::InvalidInput(msg) => msg.clone(),
            Self::PayloadTooLarge { .. } | Self::UnsupportedMediaType(_) => self.to_string(),
            // `Internal` carries the source for logging, but the response
            // surface stays generic so we don't leak details to callers.
            Self::Internal(_) => "internal server error".to_string(),
        };
        ErrorBody {
            code: self.code(),
            message,
        }
    }
}

impl From<StorageError> for ApiError {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::NotFound => Self::NotFound,
            StorageError::Conflict(msg) => Self::Conflict(msg),
            StorageError::InvalidInput(msg) => Self::InvalidInput(msg),
            // Database / migration failures are operational bugs from the
            // caller's perspective — return 500 and log the source for ops.
            StorageError::Database(e) => Self::Internal(format!("database: {e}")),
            StorageError::Migration(msg) => Self::Internal(format!("migration: {msg}")),
            // Non-exhaustive on `StorageError`: future variants land here
            // until they're given a more specific mapping above.
            other => Self::Internal(format!("storage: {other}")),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        // 5xxs almost always indicate a bug or a degraded dep; log them so
        // they show up alongside the access log. 4xx are user-driven and
        // would be noise.
        if status.is_server_error() {
            tracing::error!(error = %self, "api handler returned a 5xx");
        } else {
            tracing::debug!(error = %self, status = %status, "api handler returned a 4xx");
        }
        let body = self.body();
        (status, Json(body)).into_response()
    }
}
