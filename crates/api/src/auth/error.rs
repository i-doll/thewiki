//! Auth-layer error type and its `IntoResponse` mapping.
//!
//! [`AuthError`] is `#[non_exhaustive]` so we can grow the enum (e.g. when
//! adding 2FA in #35) without breaking downstream matchers in the same crate.
//!
//! Successful and failed login attempts return the **same response shape**
//! (HTTP 401 with body `{"error":"invalid credentials"}`) so a probe can't tell
//! whether a username exists. The discriminant is preserved in the typed
//! variant for log lines / metrics — only the wire form is fused.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thewiki_core::CaptchaError;
use thewiki_storage::StorageError;
use thiserror::Error;
use utoipa::ToSchema;

/// Wire form returned by auth endpoints on failure.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AuthErrorBody {
    /// Stable machine-readable error code.
    pub error: String,
}

/// What can go wrong on the authentication path.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AuthError {
    /// The submitted username/password combination did not match. Also used
    /// when the username doesn't exist, so the wire form is identical and a
    /// probe can't tell the two apart.
    #[error("invalid credentials")]
    InvalidCredentials,

    /// The request is missing a session cookie, or the cookie value didn't
    /// parse as a session ID.
    #[error("missing or malformed session cookie")]
    MissingSession,

    /// The session cookie resolved, but the row is gone or expired.
    #[error("session expired or revoked")]
    ExpiredSession,

    /// The authenticated user lacks the permission required by a guarded
    /// route. Distinct from `MissingSession` so we can return 403 vs 401.
    #[error("insufficient permissions")]
    Forbidden,

    /// CSRF token missing or mismatched on a mutating request.
    #[error("csrf token missing or invalid")]
    CsrfFailed,

    /// A `password-hash` operation (hash or verify) failed for a reason other
    /// than a wrong password. Typically a corrupt PHC string in the DB.
    #[error("password hashing failed: {0}")]
    HashFailure(String),

    /// Storage-layer error escaped the auth path.
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    /// Registration was attempted but the captcha token was missing or
    /// rejected (#41). Distinct from `CsrfFailed` so the SPA can surface
    /// "please complete the captcha" rather than a generic CSRF retry.
    #[error("captcha verification failed: {0}")]
    CaptchaFailed(String),

    /// The CAPTCHA upstream was unreachable. Mapped to `502 Bad Gateway`
    /// because the failure isn't on the caller.
    #[error("captcha upstream unreachable: {0}")]
    CaptchaUpstream(String),

    /// The chosen CAPTCHA provider was misconfigured at the time of the
    /// request (e.g. empty keys after a hot reload). Surfaces as `500`.
    #[error("captcha misconfigured: {0}")]
    CaptchaMisconfigured(String),

    /// Account registration is disabled in this deployment (i.e.
    /// `auth.registration = "closed"`). Renders as `403`.
    #[error("registration is closed")]
    RegistrationClosed,

    /// Caller-supplied input failed validation (e.g. invalid username,
    /// empty password). Renders as `400`.
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl From<CaptchaError> for AuthError {
    fn from(err: CaptchaError) -> Self {
        match err {
            CaptchaError::InvalidResponse(msg) => Self::CaptchaFailed(msg),
            CaptchaError::Network(msg) => Self::CaptchaUpstream(msg),
            CaptchaError::Misconfigured(msg) => Self::CaptchaMisconfigured(msg),
            // `CaptchaError` is marked `#[non_exhaustive]` so adding a new
            // variant in the trait crate doesn't break consumers. Any
            // future variant lands on the misconfigured fallback so the
            // operator sees a 500 with the unfamiliar message rather than
            // an undefined behaviour.
            other => Self::CaptchaMisconfigured(other.to_string()),
        }
    }
}

impl AuthError {
    /// HTTP status code for each variant.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        match self {
            Self::InvalidCredentials | Self::MissingSession | Self::ExpiredSession => {
                StatusCode::UNAUTHORIZED
            }
            Self::Forbidden | Self::CsrfFailed | Self::RegistrationClosed => StatusCode::FORBIDDEN,
            Self::CaptchaFailed(_) | Self::InvalidInput(_) => StatusCode::BAD_REQUEST,
            Self::CaptchaUpstream(_) => StatusCode::BAD_GATEWAY,
            Self::HashFailure(_) | Self::Storage(_) | Self::CaptchaMisconfigured(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    /// Operator-visible discriminant ("invalid_credentials", "csrf_failed",
    /// ...). The 401 variants all share `"invalid_credentials"` on the wire
    /// to avoid leaking which arm fired.
    #[must_use]
    pub fn wire_code(&self) -> &'static str {
        match self {
            Self::InvalidCredentials | Self::MissingSession | Self::ExpiredSession => {
                "invalid_credentials"
            }
            Self::Forbidden => "forbidden",
            Self::CsrfFailed => "csrf_failed",
            Self::CaptchaFailed(_) => "captcha_failed",
            Self::CaptchaUpstream(_) => "captcha_upstream",
            Self::CaptchaMisconfigured(_) => "captcha_misconfigured",
            Self::RegistrationClosed => "registration_closed",
            Self::InvalidInput(_) => "invalid_input",
            Self::HashFailure(_) | Self::Storage(_) => "internal_error",
        }
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let status = self.status();
        let code = self.wire_code();
        // Log the typed variant for operators; the wire payload stays generic.
        tracing::debug!(error = %self, code, status = %status, "auth error");
        let body = Json(AuthErrorBody {
            error: code.to_owned(),
        });
        (status, body).into_response()
    }
}
