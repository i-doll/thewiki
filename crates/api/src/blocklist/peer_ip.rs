//! Resolve the effective client IP for a request.
//!
//! Priority:
//! 1. Without [`SecurityRuntime::trust_x_forwarded_for`], or when the socket
//!    peer is not in [`SecurityRuntime::trusted_proxies`], use the socket
//!    peer (Axum's `ConnectInfo<SocketAddr>` extension).
//! 2. Otherwise, walk the `X-Forwarded-For` chain right-to-left and return
//!    the first IP that is not inside any trusted-proxy CIDR — i.e. the
//!    perceived client past the trusted hops.
//! 3. As a last-resort fallback (e.g. unit tests that bypass
//!    `into_make_service_with_connect_info`), return `127.0.0.1`.
//!
//! This is a stand-alone helper so the blocklist middleware (which runs
//! before auth + rate-limit middleware) doesn't have to import the
//! rate-limit module.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::ConnectInfo;
use axum::http::HeaderMap;
use ipnet::IpNet;

/// Lightweight snapshot of [`SecurityConfig`](crate::config::SecurityConfig)
/// with pre-parsed CIDRs.
///
/// Carved out so the middleware doesn't pay the parse cost per request and
/// so tests can construct an instance directly.
#[derive(Debug, Clone, Default)]
pub struct SecurityRuntime {
    /// When `true`, honour `X-Forwarded-For` from upstreams in
    /// [`Self::trusted_proxies`].
    pub trust_x_forwarded_for: bool,
    /// Pre-parsed CIDRs of trusted proxies.
    pub trusted_proxies: Vec<IpNet>,
}

impl SecurityRuntime {
    /// `true` if `ip` is inside any trusted-proxy CIDR.
    #[must_use]
    pub fn is_trusted_proxy(&self, ip: IpAddr) -> bool {
        self.trusted_proxies.iter().any(|net| net.contains(&ip))
    }
}

/// Resolve the effective client IP for a request.
///
/// `connect_info` is the value of `request.extensions().get::<ConnectInfo<SocketAddr>>()`
/// — passed in directly so the helper can be reused both from the
/// middleware (where the request is in hand) and from tests.
///
/// `headers` is the request header map. Only consulted when XFF is trusted.
#[must_use]
pub fn effective_client_ip(
    connect_info: Option<&ConnectInfo<SocketAddr>>,
    headers: &HeaderMap,
    runtime: &SecurityRuntime,
) -> IpAddr {
    let socket_ip = connect_info
        .map(|ConnectInfo(addr)| addr.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));

    if !runtime.trust_x_forwarded_for {
        return socket_ip;
    }
    if !runtime.is_trusted_proxy(socket_ip) {
        return socket_ip;
    }

    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| x_forwarded_for_client_ip(raw, &runtime.trusted_proxies))
        .unwrap_or(socket_ip)
}

/// Walk `raw` right-to-left and return the first IP that is not in any
/// trusted-proxy CIDR. The XFF chain is "client, proxy1, proxy2"; the right
/// edge is the closest hop and we strip those until we hit something we
/// didn't sign off on.
fn x_forwarded_for_client_ip(raw: &str, trusted_proxies: &[IpNet]) -> Option<IpAddr> {
    raw.split(',')
        .rev()
        .filter_map(parse_forwarded_ip)
        .find(|ip| !trusted_proxies.iter().any(|net| net.contains(ip)))
}

fn parse_forwarded_ip(raw: &str) -> Option<IpAddr> {
    let trimmed = raw.trim().trim_matches('"');
    trimmed.parse().ok()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use axum::http::HeaderValue;

    use super::*;

    fn runtime(trust: bool, proxies: &[&str]) -> SecurityRuntime {
        SecurityRuntime {
            trust_x_forwarded_for: trust,
            trusted_proxies: proxies.iter().map(|s| s.parse().unwrap()).collect(),
        }
    }

    fn ci(ip: &str) -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 0))
    }

    #[test]
    fn untrusted_peer_uses_socket_ip() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.7"),
        );
        let info = ci("203.0.113.5");
        let runtime = runtime(true, &["10.0.0.0/8"]);
        let ip = effective_client_ip(Some(&info), &headers, &runtime);
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5)));
    }

    #[test]
    fn xff_disabled_ignores_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.7"),
        );
        let info = ci("10.0.0.1");
        let runtime = runtime(false, &["10.0.0.0/8"]);
        let ip = effective_client_ip(Some(&info), &headers, &runtime);
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
    }

    #[test]
    fn trusted_xff_returns_client_past_proxies() {
        // Chain: client (198.51.100.7), edge proxy (10.0.0.5),
        // socket peer (10.0.0.1, also trusted). Walking right-to-left we
        // strip 10.0.0.5 (in 10.0.0.0/8) and land on 198.51.100.7.
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.7, 10.0.0.5"),
        );
        let info = ci("10.0.0.1");
        let runtime = runtime(true, &["10.0.0.0/8"]);
        let ip = effective_client_ip(Some(&info), &headers, &runtime);
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)));
    }

    #[test]
    fn spoofed_xff_ignored_when_peer_not_trusted() {
        // Even though XFF claims a private IP, the peer is the open
        // internet — we must not honour their header.
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("127.0.0.1"),
        );
        let info = ci("203.0.113.99");
        let runtime = runtime(true, &["10.0.0.0/8"]);
        let ip = effective_client_ip(Some(&info), &headers, &runtime);
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 99)));
    }

    #[test]
    fn multi_hop_xff_strips_only_trusted_hops() {
        // client, then two trusted proxies — both in 10.0.0.0/8.
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.7, 10.0.0.5, 10.0.0.6"),
        );
        let info = ci("10.0.0.1");
        let runtime = runtime(true, &["10.0.0.0/8"]);
        let ip = effective_client_ip(Some(&info), &headers, &runtime);
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)));
    }

    #[test]
    fn falls_back_to_socket_when_xff_all_trusted() {
        // No untrusted hop appears — the chain is "all proxies, all the
        // way down". The socket peer is the best information we have.
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("10.0.0.5, 10.0.0.6"),
        );
        let info = ci("10.0.0.1");
        let runtime = runtime(true, &["10.0.0.0/8"]);
        let ip = effective_client_ip(Some(&info), &headers, &runtime);
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
    }

    #[test]
    fn ipv6_socket_peer() {
        let info = ci("2001:db8::1");
        let runtime = runtime(false, &[]);
        let ip = effective_client_ip(Some(&info), &HeaderMap::new(), &runtime);
        assert_eq!(ip, IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)));
    }

    #[test]
    fn missing_connect_info_returns_loopback() {
        let runtime = runtime(false, &[]);
        let ip = effective_client_ip(None, &HeaderMap::new(), &runtime);
        assert_eq!(ip, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
}
