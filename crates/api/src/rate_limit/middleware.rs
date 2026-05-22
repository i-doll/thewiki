//! Axum middleware glue for the rate limiter.
//!
//! The middleware is a `tower::Layer`-free async fn registered with
//! `middleware::from_fn`. We do that rather than rolling a real `Layer`
//! because the wiring is one line at the call site, the layer wouldn't add
//! any reusable behaviour, and Axum's `from_fn` already gives us the right
//! request/response lifecycle.

use std::sync::Arc;

use axum::extract::{Extension, Request};
use axum::middleware::Next;
use axum::response::Response;
use tower_cookies::Cookies;

use crate::auth::AuthState;
use crate::rate_limit::config::{BucketKind, RateLimitBackendConfig, RateLimitConfig};
use crate::rate_limit::error::rate_limited_response;
use crate::rate_limit::key::{RateLimitKey, peer_ip, resolve_key};
use crate::rate_limit::store::{InMemoryRateLimitStore, RateLimitDecision, RateLimitStore};

/// Shared state for the rate-limit middleware.
///
/// `Clone`-cheap (one `Arc` per field). Constructed once at app boot and
/// inserted as an `Extension` on the router.
#[derive(Clone)]
pub struct RateLimitState {
    config: RateLimitConfig,
    store: Arc<dyn RateLimitStore>,
    auth_state: Option<AuthState>,
}

impl std::fmt::Debug for RateLimitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimitState")
            .field("config", &self.config)
            .field("auth_state", &self.auth_state.as_ref().map(|_| "<auth>"))
            .finish_non_exhaustive()
    }
}

impl RateLimitState {
    /// Build the state with the in-memory backend.
    ///
    /// This is the synchronous, infallible constructor that the `build_*`
    /// router helpers call. When the operator configures the Redis backend,
    /// `serve` calls [`Self::connect`] up front and passes the resulting
    /// state down — the routers don't need to know which backend is in use.
    ///
    /// If `config.backend` is `Redis`, the in-memory backend is used anyway
    /// (with a warning logged once) so a misconfigured router never wedges
    /// requests; the warning surfaces the misconfig in startup logs.
    #[must_use]
    pub fn new(config: RateLimitConfig, auth_state: Option<AuthState>) -> Self {
        if !matches!(config.backend, RateLimitBackendConfig::InMemory) {
            tracing::warn!(
                "rate_limit.backend is non-default but RateLimitState::new builds the in-memory \
                 store; call RateLimitState::connect to honour the configured backend"
            );
        }
        Self {
            config,
            store: Arc::new(InMemoryRateLimitStore::new()),
            auth_state,
        }
    }

    /// Async constructor that honours `config.backend`. Used by the `serve`
    /// entrypoint so a Redis-backed deploy can fail fast on a bad URL.
    ///
    /// # Errors
    ///
    /// Returns the connect failure string when the configured backend cannot
    /// be reached. The caller should surface this as a startup error rather
    /// than silently falling back — operators who asked for Redis want
    /// Redis, not a per-replica in-memory store.
    pub async fn connect(
        config: RateLimitConfig,
        auth_state: Option<AuthState>,
    ) -> Result<Self, String> {
        let store: Arc<dyn RateLimitStore> = match &config.backend {
            RateLimitBackendConfig::InMemory => Arc::new(InMemoryRateLimitStore::new()),
            #[cfg(feature = "redis")]
            RateLimitBackendConfig::Redis { url } => Arc::new(
                crate::rate_limit::store::redis::RedisRateLimitStore::connect(url)
                    .await
                    .map_err(|e| format!("redis rate-limit backend: {e}"))?,
            ),
            #[cfg(not(feature = "redis"))]
            RateLimitBackendConfig::Redis { .. } => {
                return Err(
                    "rate_limit.backend = redis requires building with --features \
                     thewiki-api/redis"
                        .to_owned(),
                );
            }
        };
        Ok(Self {
            config,
            store,
            auth_state,
        })
    }

    /// Build a state with a caller-supplied store. Useful in tests that want
    /// to inspect or pre-seed bucket state.
    #[must_use]
    pub fn with_store(
        config: RateLimitConfig,
        auth_state: Option<AuthState>,
        store: Arc<dyn RateLimitStore>,
    ) -> Self {
        Self {
            config,
            store,
            auth_state,
        }
    }

    /// Read-only access to the wrapped config (used by `app::build_full` to
    /// route based on the `enabled` switch).
    #[must_use]
    pub fn config(&self) -> &RateLimitConfig {
        &self.config
    }
}

/// Tower-style async middleware that consumes one token per request.
///
/// Wired with `middleware::from_fn` so it can be layered on any subset of
/// routes. The two-step protocol (anonymous IP check, then upgrade to user
/// key) is needed because:
///
/// 1. Resolving the session needs a storage round trip, which we do not want
///    to pay on every dropped request. The cheap IP check filters spam first.
/// 2. We must not let an attacker bypass IP limits by spamming any cookie
///    value — only a *valid* session cookie should upgrade to the (typically
///    higher) user bucket.
pub async fn rate_limit_layer(
    cookies: Cookies,
    Extension(state): Extension<RateLimitState>,
    request: Request,
    next: Next,
) -> Response {
    if !state.config.enabled {
        return next.run(request).await;
    }

    let kind = BucketKind::for_method(request.method());
    let anon_bucket = match kind {
        BucketKind::Read => state.config.read,
        BucketKind::Write => state.config.write,
    };
    let peer_ip = peer_ip(&request, &state.config);
    let ip_key = RateLimitKey::Anonymous(peer_ip);

    // Two-step protocol:
    //
    // 1. *Peek* the IP bucket (no consumption). This 429s a flood from a
    //    single IP regardless of whether they happen to carry a valid
    //    session cookie. Cheap — does not touch storage on the in-memory
    //    backend beyond a read.
    // 2. Resolve the session. If it maps to a user, the *user* bucket is
    //    charged using the (typically larger) authenticated bucket config.
    //    Otherwise the *IP* bucket is charged using the anonymous config.
    //
    // The asymmetry — peek-then-check, not check-twice — is what lets a
    // single shared IP behind NAT host many authenticated users without one
    // user starving the others.
    let preflight = match state.store.peek(ip_key, kind, anon_bucket).await {
        Ok(decision) => decision,
        Err(err) => {
            tracing::warn!(error = %err, "rate-limit peek failed; failing open");
            return next.run(request).await;
        }
    };
    if let RateLimitDecision::Denied { retry_after } = preflight {
        return rate_limited_response(retry_after);
    }

    let key = resolve_key(&cookies, state.auth_state.as_ref(), peer_ip).await;
    // Authenticated keys use the higher per-user buckets when configured;
    // anonymous keys (or auth users without a configured override) fall
    // through to the anonymous limits the IP key uses.
    let bucket = match key {
        RateLimitKey::User(_) => match kind {
            BucketKind::Read => state.config.authenticated_read.unwrap_or(anon_bucket),
            BucketKind::Write => state.config.authenticated_write.unwrap_or(anon_bucket),
        },
        RateLimitKey::Anonymous(_) => anon_bucket,
    };

    match state.store.check(key, kind, bucket).await {
        Ok(RateLimitDecision::Allowed) => next.run(request).await,
        Ok(RateLimitDecision::Denied { retry_after }) => rate_limited_response(retry_after),
        Err(err) => {
            tracing::warn!(error = %err, "rate-limit backend check failed; failing open");
            next.run(request).await
        }
    }
}
