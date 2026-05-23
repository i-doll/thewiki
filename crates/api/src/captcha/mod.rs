//! CAPTCHA provider wiring for the API layer (#41).
//!
//! The trait lives in [`thewiki_core::CaptchaProvider`]; this module ships the
//! production implementation ([`hcaptcha::HCaptcha`]), the request DTOs and
//! handlers, and the `build_provider` factory consulted at startup by
//! `serve` and the integration tests.
//!
//! Operators tune behaviour through the `[captcha]` block in
//! `thewiki.toml`. The default leaves the [`NoopCaptcha`](thewiki_core::NoopCaptcha)
//! in place so a brand-new deploy doesn't need an upstream account to boot.
//!
//! ## Module map
//!
//! - [`hcaptcha`] — production `HCaptcha` provider talking to
//!   `https://api.hcaptcha.com/siteverify`.
//! - [`routes`] — `GET /api/v1/captcha/config` so the SPA can decide whether
//!   to render the widget.
//! - [`build_provider`] — factory that consumes a [`CaptchaConfig`] and yields
//!   an `Arc<dyn CaptchaProvider>`.

use std::sync::Arc;

use thewiki_core::{CaptchaProvider, NoopCaptcha};

use crate::config::{CaptchaConfig, CaptchaProviderKind};

pub mod hcaptcha;
pub mod routes;

/// Build the [`CaptchaProvider`] dictated by the supplied config.
///
/// Returns an `Arc<dyn …>` because the provider sits in [`AppState`] and is
/// cloned cheaply into every request. The function is fallible: a config
/// asking for `hcaptcha` with an empty secret is rejected up front rather
/// than at the first request, so a misconfiguration surfaces in startup
/// logs.
///
/// # Errors
///
/// Returns an `Err` when the provider can't be built from the supplied
/// config. Today the only failing path is `hcaptcha` with an empty
/// `site_key` or `secret_key` — operators see a clean message and the
/// binary refuses to come up.
///
/// [`AppState`]: crate::state::AppState
pub fn build_provider(config: &CaptchaConfig) -> Result<Arc<dyn CaptchaProvider>, String> {
    match config.provider {
        CaptchaProviderKind::Noop => Ok(Arc::new(NoopCaptcha)),
        CaptchaProviderKind::Hcaptcha => {
            let provider = hcaptcha::HCaptcha::new(
                config.site_key.clone(),
                config.secret_key.clone(),
            )?;
            Ok(Arc::new(provider))
        }
    }
}
