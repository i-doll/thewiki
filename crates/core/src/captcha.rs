//! CAPTCHA provider abstraction (#41).
//!
//! Operators turn this on when they expose registration or anonymous edits
//! to the public internet — it's the cheapest first line of defence against
//! drive-by bot signups and edit spam. The trait is intentionally tiny:
//! given a token submitted by the browser plus the caller's IP, decide
//! whether the request is allowed through.
//!
//! ## Why a trait?
//!
//! Two reasons:
//!
//! 1. Tests want a [`NoopCaptcha`] that always accepts. Putting it behind
//!    the same trait the production code calls keeps the call site free of
//!    `#[cfg(test)]` branching.
//! 2. Operators who run behind a different CAPTCHA vendor (Cloudflare Turnstile,
//!    reCAPTCHA, …) get a single small surface to implement rather than a
//!    fork of every call site that gates an edit.
//!
//! ## dyn-compat note
//!
//! Unlike the storage repositories (which use native `async fn in trait` and
//! are therefore not `dyn`-compatible today), this trait uses [`async_trait`]
//! deliberately so the API layer can hold `Arc<dyn CaptchaProvider>` directly
//! in [`AppState`](https://docs.rs/) without parameterising every route over
//! a provider type. The performance cost (one heap allocation per `verify`
//! call) is irrelevant — `verify` already does a network round-trip.

use std::net::IpAddr;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;

/// Errors a [`CaptchaProvider`] can surface.
///
/// The variant choice maps to the HTTP status the API layer will return:
///
/// | Variant            | Meaning                                            | API status |
/// |--------------------|----------------------------------------------------|------------|
/// | [`InvalidResponse`]| The token was missing, expired, or rejected upstream. | `400`   |
/// | [`Network`]        | The upstream verifier was unreachable / 5xx.       | `502`      |
/// | [`Misconfigured`]  | The provider was used without a complete config.   | `500`      |
///
/// `#[non_exhaustive]` so new variants don't break downstream `match` arms.
///
/// [`InvalidResponse`]: CaptchaError::InvalidResponse
/// [`Network`]: CaptchaError::Network
/// [`Misconfigured`]: CaptchaError::Misconfigured
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CaptchaError {
    /// The CAPTCHA challenge response was missing, malformed, or rejected by
    /// the upstream verifier.
    ///
    /// Carries the provider-supplied reason (e.g. hCaptcha's `error-codes`
    /// list joined with commas) so the API layer can log it for operators
    /// debugging a rejection storm.
    #[error("invalid captcha response: {0}")]
    InvalidResponse(String),

    /// The upstream verifier was unreachable, timed out, or returned a 5xx.
    /// Distinct from [`Self::InvalidResponse`] so the API layer can return a
    /// `502 Bad Gateway` instead of pinning the failure on the caller.
    #[error("captcha network error: {0}")]
    Network(String),

    /// The provider was used without a complete configuration (e.g. an
    /// hCaptcha instance with an empty secret). This is an operator
    /// misconfiguration, not a caller-facing error.
    #[error("captcha provider misconfigured: {0}")]
    Misconfigured(String),
}

/// Public configuration the SPA needs to render a CAPTCHA challenge.
///
/// Surfaced through `GET /api/v1/captcha/config`. `provider` is a stable
/// short identifier (`"hcaptcha"`, `"turnstile"`, …) the SPA branches on to
/// pick the right widget; `site_key` is the public key the widget receives.
///
/// Sensitive material (the secret) is never included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CaptchaFrontendConfig {
    /// Short stable identifier the SPA matches on (e.g. `"hcaptcha"`).
    pub provider: String,
    /// Public site key. Safe to embed in HTML / JS — never the secret.
    pub site_key: String,
}

/// Server-side CAPTCHA verifier.
///
/// Implementations must be `Send + Sync` so they can sit behind an `Arc`
/// in [`AppState`](https://docs.rs/) and be cloned cheaply into every
/// request.
#[async_trait]
pub trait CaptchaProvider: Send + Sync {
    /// Validate a `response` token submitted by the browser.
    ///
    /// `remote_ip`, when supplied, is forwarded to the verifier so it can
    /// correlate the challenge with the originating client (some providers
    /// require it for replay protection). The API layer pulls it off the
    /// request's connection info; tests typically pass `None`.
    ///
    /// # Errors
    ///
    /// - [`CaptchaError::InvalidResponse`] when the token is missing,
    ///   malformed, expired, or rejected upstream.
    /// - [`CaptchaError::Network`] when the upstream verifier is unreachable
    ///   or returns a 5xx.
    /// - [`CaptchaError::Misconfigured`] when the provider was constructed
    ///   without the bits it needs to talk to the upstream.
    async fn verify(
        &self,
        response: &str,
        remote_ip: Option<IpAddr>,
    ) -> Result<(), CaptchaError>;

    /// Frontend embed config, or `None` when the provider does not render a
    /// widget (e.g. [`NoopCaptcha`]).
    ///
    /// The SPA fetches this on app boot and skips rendering the challenge
    /// when it gets `None`, so the developer experience stays "just works"
    /// in default deploys.
    fn frontend_config(&self) -> Option<CaptchaFrontendConfig>;
}

/// No-op provider that accepts every token. Used as the default and in
/// tests that don't exercise the CAPTCHA path.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopCaptcha;

#[async_trait]
impl CaptchaProvider for NoopCaptcha {
    async fn verify(
        &self,
        _response: &str,
        _remote_ip: Option<IpAddr>,
    ) -> Result<(), CaptchaError> {
        Ok(())
    }

    fn frontend_config(&self) -> Option<CaptchaFrontendConfig> {
        None
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_accepts_any_token() {
        let noop = NoopCaptcha;
        noop.verify("", None).await.expect("empty token");
        noop.verify("anything", None).await.expect("opaque token");
        noop.verify(
            "with-ip",
            Some(IpAddr::from([127, 0, 0, 1])),
        )
        .await
        .expect("with ip");
    }

    #[test]
    fn noop_publishes_no_frontend_config() {
        assert!(NoopCaptcha.frontend_config().is_none());
    }
}
