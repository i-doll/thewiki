//! Rate-limit key extraction.
//!
//! A [`RateLimitKey`] identifies the principal a request is charged to:
//!
//! - [`RateLimitKey::User`] — an authenticated session was present and resolved
//!   to a [`UserId`]. The user is charged regardless of the source IP, so a
//!   single user roaming across networks shares one bucket.
//! - [`RateLimitKey::Anonymous`] — fallback for requests without a valid session.
//!   The key is the perceived client IP, which is the socket peer unless the
//!   request came from a trusted proxy and a configured forwarding header is
//!   present (see [`peer_ip`]).
//!
//! The session lookup is deliberately tolerant: a missing/expired/unknown
//! session falls back to IP keying instead of rejecting the request. The
//! resolved [`AuthSession`](crate::auth::AuthSession) extractor itself remains
//! the authoritative gate for protected handlers — this layer is purely about
//! who-to-charge.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::{ConnectInfo, Request};
use thewiki_core::{SessionId, UserId};
use thewiki_storage::StorageError;
use thewiki_storage::repo::SessionRepository;
use tower_cookies::Cookies;

use crate::auth::AuthState;
use crate::auth::session::{SESSION_COOKIE, decode_session_id};
use crate::rate_limit::config::{ClientIpHeader, RateLimitConfig};

/// The principal a request is charged to.
///
/// Cheap to copy (one IP or UUID-sized value) — kept that way so the map key
/// type stays `Copy + Hash` and `DashMap` lookups don't allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitKey {
    /// Anonymous request keyed by the perceived client IP.
    Anonymous(IpAddr),
    /// Authenticated request keyed by the user ID.
    User(UserId),
}

/// Resolve the principal for `request`.
///
/// Looks for a session cookie; if present, attempts to map it to a user via
/// the configured [`AuthState`]. Storage errors (other than `NotFound`) are
/// logged and fall through to the IP path — failing closed here would couple
/// rate limiting availability to session storage availability, which makes
/// the limiter a single point of failure.
pub async fn resolve_key(
    cookies: &Cookies,
    auth_state: Option<&AuthState>,
    peer_ip: IpAddr,
) -> RateLimitKey {
    let Some(session_id) = session_id_from_cookies(cookies) else {
        return RateLimitKey::Anonymous(peer_ip);
    };

    let Some(auth_state) = auth_state else {
        return RateLimitKey::Anonymous(peer_ip);
    };

    match auth_state.storage.sessions().get_by_id(session_id).await {
        Ok(session) => RateLimitKey::User(session.user_id),
        Err(StorageError::NotFound) => RateLimitKey::Anonymous(peer_ip),
        Err(e) => {
            tracing::warn!(error = %e, "rate-limit session lookup failed");
            RateLimitKey::Anonymous(peer_ip)
        }
    }
}

/// Read the `thewiki_session` cookie and decode the opaque session ID.
fn session_id_from_cookies(cookies: &Cookies) -> Option<SessionId> {
    cookies
        .get(SESSION_COOKIE)
        .and_then(|cookie| decode_session_id(cookie.value()))
}

/// Determine the IP we will rate-limit anonymous requests under.
///
/// Priority:
/// 1. If the request did not arrive via a `ConnectInfo<SocketAddr>` (e.g.
///    in unit tests that bypass `into_make_service_with_connect_info`), use
///    `127.0.0.1`. Production callers always have `ConnectInfo` populated.
/// 2. If the socket peer is not in `trusted_proxies`, ignore any forwarding
///    headers and return the socket peer directly. This is the safe default —
///    spoofing `X-Forwarded-For` from an arbitrary client must not be honoured.
/// 3. Otherwise, consult the configured `client_ip_header` and return the first
///    parsed IP, falling back to the socket peer if the header is missing or
///    malformed.
#[must_use]
pub fn peer_ip(request: &Request, config: &RateLimitConfig) -> IpAddr {
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

/// Scan an `X-Forwarded-For` header right-to-left and return the first
/// address that is not a trusted proxy. Standard behaviour for chains like
/// `client, proxy1, proxy2`: we walk back past the proxies to find the
/// closest IP we trust as actually-the-client.
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
