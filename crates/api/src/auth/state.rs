//! Typed application state for the auth layer.
//!
//! [`AuthState`] carries the pieces the auth handlers and extractors need —
//! the storage facade, the configured password hasher, and the session TTL.
//! It is `Clone`-cheap (everything inside is an `Arc` or a `Copy` primitive)
//! and lives in axum's `State<AuthState>`.

use std::sync::Arc;
use std::time::Duration;

use thewiki_core::{CaptchaProvider, NoopCaptcha};
use thewiki_storage::sqlite::SqliteStorage;

use crate::auth::password::Argon2Hasher;
use crate::config::{AuthConfig, CaptchaConfig};

/// Auth-related app state.
///
/// `storage` is the concrete SQLite facade. We could narrow it to a set of
/// repository handles, but the trait + opaque `Arc<dyn …>` route doesn't
/// work today (see the comment block in
/// [`thewiki_storage::repo`](thewiki_storage::repo)), so a concrete struct
/// is the simpler call. Swapping in `postgres`/`libsql` at M1 means swapping
/// the field type — the handlers stay the same.
#[derive(Clone)]
pub struct AuthState {
    /// Storage facade. Cloning is cheap — the inner `SqlitePool` is an `Arc`.
    pub storage: SqliteStorage,
    /// Argon2id hasher wired from `auth.argon2`.
    pub hasher: Arc<Argon2Hasher>,
    /// Per-session lifetime; passed to the session-issuance call.
    pub session_ttl: Duration,
    /// `Secure` cookie attribute switch. `true` in production / when behind
    /// TLS; `false` for local development over plain HTTP. Defaults to `true`
    /// — operators flip it via the `--insecure-cookie` CLI flag.
    pub secure_cookies: bool,
    /// Snapshot of `Config::auth` — published verbatim by
    /// `GET /api/v1/auth/policy` so the SPA can surface the right login /
    /// signup affordances. Also consulted by the configurable-auth extractors
    /// via the `AppState` indirection.
    pub config: AuthConfig,
    /// CAPTCHA provider (#41). Consulted by the register handler when
    /// `captcha.apply_to_registration = true`. Defaults to the noop
    /// provider in tests and integration fixtures that don't stand up a
    /// real provider.
    pub captcha: Arc<dyn CaptchaProvider>,
    /// Snapshot of `Config::captcha` — handlers branch on `apply_to_*`
    /// flags to decide whether to require a token before mutating state.
    pub captcha_config: CaptchaConfig,
}

impl std::fmt::Debug for AuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `dyn CaptchaProvider` doesn't carry a `Debug` bound — the trait
        // is consumed by `async_trait` machinery and forcing `Debug` on
        // every impl would be a pointless constraint. We synthesise a
        // placeholder so the field name stays visible in panic backtraces.
        //
        // CAPTCHA config is projected explicitly rather than forwarded
        // wholesale: `secret_key` is a sensitive credential and the
        // derived `Debug` on `CaptchaConfig` would dump it verbatim into
        // any log line, panic backtrace, or `dbg!(...)` call. We surface
        // only the provider name, presence booleans for the keys, and the
        // operator-controlled `apply_to_*` flags.
        f.debug_struct("AuthState")
            .field("storage", &self.storage)
            .field("hasher", &self.hasher)
            .field("session_ttl", &self.session_ttl)
            .field("secure_cookies", &self.secure_cookies)
            .field("config", &self.config)
            .field("captcha", &"<dyn CaptchaProvider>")
            .field("captcha_provider", &self.captcha_config.provider)
            .field(
                "captcha_site_key_configured",
                &!self.captcha_config.site_key.trim().is_empty(),
            )
            .field(
                "captcha_secret_key_configured",
                &!self.captcha_config.secret_key.trim().is_empty(),
            )
            .field(
                "captcha_apply_to_registration",
                &self.captcha_config.apply_to_registration,
            )
            .field(
                "captcha_apply_to_anonymous_edits",
                &self.captcha_config.apply_to_anonymous_edits,
            )
            .finish()
    }
}

impl AuthState {
    /// Build the state from its parts.
    ///
    /// The CAPTCHA wiring defaults to the noop provider so existing callers
    /// (integration tests, the auth-only app constructor) don't have to
    /// know about it. Production wires the operator-configured provider
    /// via [`Self::with_captcha`].
    #[must_use]
    pub fn new(
        storage: SqliteStorage,
        hasher: Arc<Argon2Hasher>,
        session_ttl: Duration,
        secure_cookies: bool,
        config: AuthConfig,
    ) -> Self {
        Self {
            storage,
            hasher,
            session_ttl,
            secure_cookies,
            config,
            captcha: Arc::new(NoopCaptcha),
            captcha_config: CaptchaConfig::default(),
        }
    }

    /// Wire the CAPTCHA provider + config snapshot.
    #[must_use]
    pub fn with_captcha(
        mut self,
        captcha_config: CaptchaConfig,
        provider: Arc<dyn CaptchaProvider>,
    ) -> Self {
        self.captcha = provider;
        self.captcha_config = captcha_config;
        self
    }
}
