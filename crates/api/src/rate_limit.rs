//! Token-bucket rate limiting middleware.
//!
//! Buckets are keyed by request class (read/write) and principal. A valid
//! session cookie maps to the user ID; otherwise the remote peer IP is used.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{ConnectInfo, Extension, Request};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use thewiki_core::{SessionId, UserId};
use thewiki_storage::StorageError;
use thewiki_storage::repo::SessionRepository;
use tower_cookies::Cookies;
use utoipa::ToSchema;

use crate::auth::AuthState;
use crate::auth::session::{SESSION_COOKIE, decode_session_id};
use crate::config::{
    ClientIpHeader, RateLimitBackendConfig, RateLimitBucketConfig, RateLimitConfig,
};

/// Wire form returned when a request exceeds its configured rate limit.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RateLimitErrorBody {
    /// Stable machine-readable error code.
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BucketKind {
    Read,
    Write,
}

impl BucketKind {
    #[must_use]
    pub fn for_method(method: &Method) -> Self {
        if matches!(method, &Method::GET | &Method::HEAD | &Method::OPTIONS) {
            Self::Read
        } else {
            Self::Write
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitPrincipal {
    User(UserId),
    Ip(IpAddr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RateLimitKey {
    pub kind: BucketKind,
    pub principal: RateLimitPrincipal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitDecision {
    Allowed,
    Denied { retry_after: Duration },
}

pub trait RateLimitStore: Send + Sync + 'static {
    fn peek(
        &self,
        key: RateLimitKey,
        bucket: RateLimitBucketConfig,
        now: Instant,
    ) -> RateLimitDecision;

    fn check(
        &self,
        key: RateLimitKey,
        bucket: RateLimitBucketConfig,
        now: Instant,
    ) -> RateLimitDecision;
}

#[derive(Debug, Default)]
pub struct InMemoryRateLimitStore {
    buckets: Mutex<HashMap<RateLimitKey, BucketState>>,
}

#[derive(Debug, Clone, Copy)]
struct BucketState {
    tokens: f64,
    last_refill: Instant,
    bucket: RateLimitBucketConfig,
}

impl InMemoryRateLimitStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    #[must_use]
    pub fn check_at(
        &self,
        key: RateLimitKey,
        bucket: RateLimitBucketConfig,
        now: Instant,
    ) -> RateLimitDecision {
        self.check(key, bucket, now)
    }
}

impl RateLimitStore for InMemoryRateLimitStore {
    fn peek(
        &self,
        key: RateLimitKey,
        bucket: RateLimitBucketConfig,
        now: Instant,
    ) -> RateLimitDecision {
        let mut buckets = match self.buckets.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // TODO(#35): this O(n) sweep is fine for the in-memory MVP, but a
        // shared/high-scale backend should move pruning to a background or
        // probabilistic cleanup path.
        prune_expired_buckets(&mut buckets, now);
        let Some(state) = buckets.get(&key) else {
            return RateLimitDecision::Allowed;
        };

        let mut preview = *state;
        preview.bucket = bucket;
        refill(&mut preview, bucket, now);

        if preview.tokens >= 1.0 {
            RateLimitDecision::Allowed
        } else {
            RateLimitDecision::Denied {
                retry_after: retry_after(preview.tokens, bucket),
            }
        }
    }

    fn check(
        &self,
        key: RateLimitKey,
        bucket: RateLimitBucketConfig,
        now: Instant,
    ) -> RateLimitDecision {
        let mut buckets = match self.buckets.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // TODO(#35): this O(n) sweep is fine for the in-memory MVP, but a
        // shared/high-scale backend should move pruning to a background or
        // probabilistic cleanup path.
        prune_expired_buckets(&mut buckets, now);
        let state = buckets.entry(key).or_insert(BucketState {
            tokens: f64::from(bucket.capacity),
            last_refill: now,
            bucket,
        });
        state.bucket = bucket;

        refill(state, bucket, now);

        if state.tokens >= 1.0 {
            state.tokens -= 1.0;
            RateLimitDecision::Allowed
        } else {
            RateLimitDecision::Denied {
                retry_after: retry_after(state.tokens, bucket),
            }
        }
    }
}

fn prune_expired_buckets(buckets: &mut HashMap<RateLimitKey, BucketState>, now: Instant) {
    buckets.retain(|_, state| !is_idle_expired(*state, state.bucket, now));
}

fn is_idle_expired(state: BucketState, bucket: RateLimitBucketConfig, now: Instant) -> bool {
    if now <= state.last_refill {
        return false;
    }

    let elapsed = now.duration_since(state.last_refill).as_secs_f64();
    let interval = Duration::from_secs(bucket.refill_interval_secs).as_secs_f64();
    let refill_rate = f64::from(bucket.refill_tokens) / interval;
    let missing_tokens = (f64::from(bucket.capacity) - state.tokens).max(0.0);
    let seconds_to_full = missing_tokens / refill_rate;

    elapsed >= seconds_to_full + interval
}

fn refill(state: &mut BucketState, bucket: RateLimitBucketConfig, now: Instant) {
    if now <= state.last_refill {
        return;
    }

    let elapsed = now.duration_since(state.last_refill).as_secs_f64();
    let interval = Duration::from_secs(bucket.refill_interval_secs).as_secs_f64();
    let refill_rate = f64::from(bucket.refill_tokens) / interval;
    state.tokens = f64::from(bucket.capacity).min(state.tokens + elapsed * refill_rate);
    state.last_refill = now;
}

fn retry_after(tokens: f64, bucket: RateLimitBucketConfig) -> Duration {
    let interval = Duration::from_secs(bucket.refill_interval_secs).as_secs_f64();
    let refill_rate = f64::from(bucket.refill_tokens) / interval;
    let seconds = ((1.0 - tokens).max(0.0) / refill_rate).ceil().max(1.0) as u64;
    Duration::from_secs(seconds)
}

#[derive(Clone)]
pub struct RateLimitState {
    config: RateLimitConfig,
    store: Arc<dyn RateLimitStore>,
    auth_state: Option<AuthState>,
}

impl RateLimitState {
    #[must_use]
    pub fn new(config: RateLimitConfig, auth_state: Option<AuthState>) -> Self {
        let store: Arc<dyn RateLimitStore> = match config.backend {
            RateLimitBackendConfig::InMemory => Arc::new(InMemoryRateLimitStore::new()),
        };
        Self {
            config,
            store,
            auth_state,
        }
    }

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
}

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
    let bucket = match kind {
        BucketKind::Read => state.config.read,
        BucketKind::Write => state.config.write,
    };
    let peer_ip = peer_ip(&request, &state.config);
    let ip_key = RateLimitKey {
        kind,
        principal: RateLimitPrincipal::Ip(peer_ip),
    };
    let now = Instant::now();

    let session_id = session_id_from_cookies(&cookies);
    let Some(session_id) = session_id else {
        return match state.store.check(ip_key, bucket, now) {
            RateLimitDecision::Allowed => next.run(request).await,
            RateLimitDecision::Denied { retry_after } => rate_limited_response(retry_after),
        };
    };

    match state.store.peek(ip_key, bucket, now) {
        RateLimitDecision::Allowed => {}
        RateLimitDecision::Denied { retry_after } => return rate_limited_response(retry_after),
    }

    let principal = resolve_principal(&state.auth_state, Some(session_id), peer_ip).await;
    let key = RateLimitKey { kind, principal };

    match state.store.check(key, bucket, now) {
        RateLimitDecision::Allowed => next.run(request).await,
        RateLimitDecision::Denied { retry_after } => rate_limited_response(retry_after),
    }
}

async fn resolve_principal(
    auth_state: &Option<AuthState>,
    session_id: Option<SessionId>,
    peer_ip: IpAddr,
) -> RateLimitPrincipal {
    if let Some(auth_state) = auth_state
        && let Some(session_id) = session_id
    {
        match auth_state.storage.sessions().get_by_id(session_id).await {
            Ok(session) => return RateLimitPrincipal::User(session.user_id),
            Err(StorageError::NotFound) => {}
            Err(e) => tracing::warn!(error = %e, "rate-limit session lookup failed"),
        }
    }

    RateLimitPrincipal::Ip(peer_ip)
}

fn session_id_from_cookies(cookies: &Cookies) -> Option<SessionId> {
    cookies
        .get(SESSION_COOKIE)
        .and_then(|cookie| decode_session_id(cookie.value()))
}

fn peer_ip(request: &Request, config: &RateLimitConfig) -> IpAddr {
    let socket_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));

    if !config.trusted_proxies.contains(&socket_ip) {
        return socket_ip;
    }

    let Some(header) = config.client_ip_header else {
        return socket_ip;
    };

    forwarded_ip(request, header, config).unwrap_or(socket_ip)
}

fn forwarded_ip(
    request: &Request,
    header: ClientIpHeader,
    config: &RateLimitConfig,
) -> Option<IpAddr> {
    match header {
        ClientIpHeader::XForwardedFor => request
            .headers()
            .get("x-forwarded-for")?
            .to_str()
            .ok()
            .and_then(|raw| x_forwarded_for_ip(raw, &config.trusted_proxies)),
        ClientIpHeader::XRealIp => request
            .headers()
            .get("x-real-ip")?
            .to_str()
            .ok()
            .and_then(parse_forwarded_ip),
    }
}

fn x_forwarded_for_ip(raw: &str, trusted_proxies: &[IpAddr]) -> Option<IpAddr> {
    raw.split(',')
        .rev()
        .filter_map(parse_forwarded_ip)
        .find(|ip| !trusted_proxies.contains(ip))
}

fn parse_forwarded_ip(raw: &str) -> Option<IpAddr> {
    let trimmed = raw.trim().trim_matches('"');
    trimmed.parse().ok()
}

fn rate_limited_response(retry_after: Duration) -> Response {
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn bucket() -> RateLimitBucketConfig {
        RateLimitBucketConfig {
            capacity: 1,
            refill_tokens: 1,
            refill_interval_secs: 2,
        }
    }

    fn key() -> RateLimitKey {
        RateLimitKey {
            kind: BucketKind::Read,
            principal: RateLimitPrincipal::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        }
    }

    #[test]
    fn token_bucket_exhausts_and_refills() {
        let store = InMemoryRateLimitStore::new();
        let now = Instant::now();

        assert_eq!(
            store.check_at(key(), bucket(), now),
            RateLimitDecision::Allowed
        );
        assert_eq!(
            store.check_at(key(), bucket(), now),
            RateLimitDecision::Denied {
                retry_after: Duration::from_secs(2)
            }
        );
        assert_eq!(
            store.check_at(key(), bucket(), now + Duration::from_secs(2)),
            RateLimitDecision::Allowed
        );
    }

    #[test]
    fn idle_buckets_expire_after_refill_grace_period() {
        let store = InMemoryRateLimitStore::new();
        let now = Instant::now();
        let first = RateLimitKey {
            kind: BucketKind::Write,
            principal: RateLimitPrincipal::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        };
        let second = RateLimitKey {
            kind: BucketKind::Read,
            principal: RateLimitPrincipal::Ip(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))),
        };

        assert_eq!(
            store.check_at(first, bucket(), now),
            RateLimitDecision::Allowed
        );
        assert_eq!(
            store.check_at(second, bucket(), now + Duration::from_secs(4)),
            RateLimitDecision::Allowed
        );
        let buckets = store.buckets.lock().expect("buckets lock");
        assert_eq!(buckets.len(), 1);
        assert!(buckets.contains_key(&second));
    }
}
