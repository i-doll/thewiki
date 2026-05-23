//! hCaptcha implementation of [`CaptchaProvider`].
//!
//! Verifies tokens against `https://api.hcaptcha.com/siteverify` using a
//! plain form-encoded POST. The site key is published to the SPA via
//! [`CaptchaFrontendConfig`] so the widget can render; the secret is held
//! server-side and only crosses the wire to hCaptcha.
//!
//! ## Wire shape
//!
//! Request body (form-encoded):
//!
//! ```text
//! secret=<server-side secret>
//! response=<browser token>
//! remoteip=<optional caller ip>
//! ```
//!
//! Response (JSON):
//!
//! ```json
//! { "success": true | false, "error-codes": ["missing-input-response", ...] }
//! ```
//!
//! Non-success responses surface as [`CaptchaError::InvalidResponse`] with
//! the `error-codes` list joined into the message so operators can grep
//! logs for `expired-input-response` etc. Transport failures (DNS, connection
//! reset, 5xx) become [`CaptchaError::Network`] so the API layer can map
//! them to a `502 Bad Gateway` rather than blaming the caller.

use std::net::IpAddr;

use async_trait::async_trait;
use serde::Deserialize;
use thewiki_core::{CaptchaError, CaptchaFrontendConfig, CaptchaProvider};

/// Stable identifier used on the wire (`/api/v1/captcha/config`) so the SPA
/// can branch on which widget to mount. Bumping this is a wire-breaking
/// change for the SPA, so it lives as a `const` rather than getting
/// repeated as a string literal.
pub const PROVIDER_KIND: &str = "hcaptcha";

/// Official upstream verifier endpoint. Pulled into a const so tests can
/// override it via [`HCaptcha::with_endpoint`].
pub const SITEVERIFY_URL: &str = "https://api.hcaptcha.com/siteverify";

/// hCaptcha-backed CAPTCHA provider.
///
/// `site_key` is safe to publish — it identifies the operator's hCaptcha
/// site to the rendered widget. `secret_key` is held server-side and only
/// crosses the wire to hCaptcha during `verify`.
#[derive(Debug, Clone)]
pub struct HCaptcha {
    site_key: String,
    secret_key: String,
    /// Overridable so tests can point at a `wiremock` server instead of
    /// the real upstream. Production callers go through [`HCaptcha::new`]
    /// which hard-codes [`SITEVERIFY_URL`].
    endpoint: String,
    client: reqwest::Client,
}

impl HCaptcha {
    /// Build a provider with the supplied keys. Both must be non-empty —
    /// a partially configured deploy is an operator bug and we surface
    /// it loudly here rather than at the first failed verify.
    ///
    /// # Errors
    ///
    /// Returns the misconfiguration message when either key is empty after
    /// trimming whitespace.
    pub fn new(site_key: String, secret_key: String) -> Result<Self, String> {
        if site_key.trim().is_empty() {
            return Err("captcha.site_key must be non-empty for provider = \"hcaptcha\"".to_string());
        }
        if secret_key.trim().is_empty() {
            return Err(
                "captcha.secret_key must be non-empty for provider = \"hcaptcha\"".to_string(),
            );
        }
        // We keep a single `reqwest::Client` so connection pooling carries
        // across requests. Construction is infallible for the default
        // configuration (rustls TLS, no proxies, default timeouts).
        let client = reqwest::Client::builder()
            // 5s is generous for a single form POST; hCaptcha typically
            // responds in <100ms. We bound it so a degraded upstream
            // doesn't starve the request worker.
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| format!("building captcha http client: {e}"))?;
        Ok(Self {
            site_key,
            secret_key,
            endpoint: SITEVERIFY_URL.to_string(),
            client,
        })
    }

    /// Override the upstream endpoint. Used by tests only; production
    /// callers should keep the default [`SITEVERIFY_URL`].
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }
}

/// Wire shape of the upstream JSON response.
///
/// hCaptcha returns several other fields (`hostname`, `credit`,
/// `challenge_ts`) but we only consume the two that drive the
/// success/failure decision.
#[derive(Debug, Deserialize)]
struct SiteVerifyResponse {
    success: bool,
    #[serde(default, rename = "error-codes")]
    error_codes: Vec<String>,
}

#[async_trait]
impl CaptchaProvider for HCaptcha {
    async fn verify(
        &self,
        response: &str,
        remote_ip: Option<IpAddr>,
    ) -> Result<(), CaptchaError> {
        if response.trim().is_empty() {
            return Err(CaptchaError::InvalidResponse(
                "missing-input-response".to_string(),
            ));
        }

        // Form-encoded body per the hCaptcha docs. We send `remoteip` only
        // when the caller supplied one — passing an empty value confuses
        // the upstream, which prefers the field absent in that case.
        let mut form: Vec<(&str, String)> = vec![
            ("secret", self.secret_key.clone()),
            ("response", response.to_string()),
        ];
        if let Some(ip) = remote_ip {
            form.push(("remoteip", ip.to_string()));
        }

        let res = self
            .client
            .post(&self.endpoint)
            .form(&form)
            .send()
            .await
            .map_err(|e| CaptchaError::Network(format!("siteverify request: {e}")))?;

        if !res.status().is_success() {
            return Err(CaptchaError::Network(format!(
                "siteverify returned HTTP {}",
                res.status()
            )));
        }

        let body: SiteVerifyResponse = res
            .json()
            .await
            .map_err(|e| CaptchaError::Network(format!("siteverify body: {e}")))?;

        if body.success {
            Ok(())
        } else {
            // Surface the upstream's reason list joined with commas. The
            // empty-vec case ("success: false" with no codes) shouldn't
            // happen against hCaptcha but is defensive.
            let detail = if body.error_codes.is_empty() {
                "rejected".to_string()
            } else {
                body.error_codes.join(",")
            };
            Err(CaptchaError::InvalidResponse(detail))
        }
    }

    fn frontend_config(&self) -> Option<CaptchaFrontendConfig> {
        Some(CaptchaFrontendConfig {
            provider: PROVIDER_KIND.to_string(),
            site_key: self.site_key.clone(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "ergonomic tests")]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn provider(endpoint: String) -> HCaptcha {
        HCaptcha::new("site-key".to_string(), "secret-key".to_string())
            .expect("provider builds with both keys set")
            .with_endpoint(endpoint)
    }

    #[tokio::test]
    async fn new_rejects_empty_site_key() {
        let err = HCaptcha::new(String::new(), "secret".to_string()).expect_err("empty site key");
        assert!(err.contains("site_key"), "{err}");
    }

    #[tokio::test]
    async fn new_rejects_empty_secret_key() {
        let err = HCaptcha::new("site".to_string(), "  ".to_string()).expect_err("empty secret");
        assert!(err.contains("secret_key"), "{err}");
    }

    #[tokio::test]
    async fn verify_accepts_success_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
            })))
            .mount(&server)
            .await;

        let p = provider(server.uri());
        p.verify("real-token", None).await.expect("success");
    }

    #[tokio::test]
    async fn verify_rejects_failure_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": false,
                "error-codes": ["invalid-input-response", "missing-input-secret"],
            })))
            .mount(&server)
            .await;

        let p = provider(server.uri());
        let err = p.verify("bad-token", None).await.expect_err("failure body");
        match err {
            CaptchaError::InvalidResponse(msg) => {
                assert!(msg.contains("invalid-input-response"), "{msg}");
                assert!(msg.contains("missing-input-secret"), "{msg}");
            }
            other => panic!("expected InvalidResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_rejects_failure_with_empty_codes() {
        // Defensive: hCaptcha shouldn't return success=false without codes,
        // but if it does we still want a useful error message.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": false,
            })))
            .mount(&server)
            .await;

        let p = provider(server.uri());
        let err = p.verify("anything", None).await.expect_err("failure body");
        assert!(matches!(err, CaptchaError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn verify_treats_5xx_as_network_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let p = provider(server.uri());
        let err = p.verify("token", None).await.expect_err("5xx");
        assert!(matches!(err, CaptchaError::Network(_)), "{err:?}");
    }

    #[tokio::test]
    async fn verify_treats_unreachable_upstream_as_network_error() {
        // Point at an endpoint that's guaranteed to be closed — we don't
        // care about the exact failure mode (connection refused / TCP
        // reset / etc.), only that it surfaces as `CaptchaError::Network`.
        let p = provider("http://127.0.0.1:1/siteverify".to_string());
        let err = p
            .verify("token", None)
            .await
            .expect_err("unreachable upstream");
        assert!(matches!(err, CaptchaError::Network(_)), "{err:?}");
    }

    #[tokio::test]
    async fn verify_rejects_empty_token_locally() {
        // We don't burn an upstream call for an obviously-missing token:
        // the provider short-circuits with `missing-input-response`. The
        // mock server is configured to fail the assertion if it's hit.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true,
            })))
            .expect(0)
            .mount(&server)
            .await;

        let p = provider(server.uri());
        let err = p.verify("", None).await.expect_err("empty token");
        match err {
            CaptchaError::InvalidResponse(msg) => {
                assert_eq!(msg, "missing-input-response");
            }
            other => panic!("expected InvalidResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn frontend_config_publishes_site_key() {
        let p =
            HCaptcha::new("public-site-key".to_string(), "secret".to_string()).expect("provider");
        let cfg = p.frontend_config().expect("hcaptcha publishes config");
        assert_eq!(cfg.provider, "hcaptcha");
        assert_eq!(cfg.site_key, "public-site-key");
    }
}
