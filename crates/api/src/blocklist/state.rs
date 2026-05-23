//! In-memory blocklist snapshot held under a `tokio::sync::RwLock`.
//!
//! The snapshot is hydrated from storage on boot and refreshed on every
//! admin mutation. Reads (which run on every request) take a read lock,
//! clone the cheap `Arc<Snapshot>`, and drop the guard — so the hot path
//! is effectively wait-free.
//!
//! Why an `Arc<Snapshot>` swap instead of a plain `RwLock<Snapshot>`?
//! The snapshot contains a compiled `regex::RegexSet`, which is moderately
//! expensive to build. Holding a read lock for the duration of a request
//! would block admin mutations behind every slow request; instead we
//! `Arc::clone` the snapshot pointer (cheap), release the lock immediately,
//! and operate on the captured snapshot without lock contention.

use std::net::IpAddr;
use std::sync::Arc;

use ipnet::IpNet;
use regex::RegexSet;
use thewiki_storage::StorageError;
use thewiki_storage::repo::{IpBlocklistRepository, UrlBlocklistRepository};
use tokio::sync::RwLock;

/// Immutable snapshot of the compiled blocklists.
///
/// One instance is built per refresh. Lookups are read-only, so the snapshot
/// is shared through an `Arc` and any request can hold its own clone for as
/// long as it likes without blocking writers.
#[derive(Debug, Clone, Default)]
pub struct BlocklistSnapshot {
    /// Parsed CIDR ranges. Linear scan is fine for v1 — the operator-curated
    /// list will be in the low hundreds at most.
    pub ip_nets: Vec<IpNet>,
    /// Pre-compiled URL pattern set. `is_match` short-circuits at the first
    /// hit; iterating matches happens only when the page edit handler wants
    /// to render the offending entry.
    ///
    /// Stored alongside [`url_patterns`](Self::url_patterns) so the matched
    /// index can be mapped back to the human-readable source pattern.
    pub url_regex_set: RegexSet,
    /// Source patterns aligned with `url_regex_set` (same index order). Used
    /// to render `400 invalid_input` bodies that point at the offending
    /// pattern.
    pub url_patterns: Vec<String>,
}

impl BlocklistSnapshot {
    /// `true` if `ip` falls inside any of the configured CIDRs.
    #[must_use]
    pub fn contains_ip(&self, ip: IpAddr) -> bool {
        self.ip_nets.iter().any(|net| net.contains(&ip))
    }

    /// Return every URL pattern (as human-readable source string) that
    /// matches `url`. `None` is returned for the empty result so callers
    /// can branch with `if let Some(_)`.
    #[must_use]
    pub fn matching_patterns(&self, url: &str) -> Option<Vec<&str>> {
        let hits: Vec<_> = self
            .url_regex_set
            .matches(url)
            .into_iter()
            .filter_map(|idx| self.url_patterns.get(idx).map(String::as_str))
            .collect();
        if hits.is_empty() { None } else { Some(hits) }
    }
}

/// Wrapper around the snapshot pointer that the API state shares with the
/// admin handlers + the middleware.
///
/// Clones are cheap (a single `Arc::clone`) so the state can be carried
/// through axum extensions without contention.
#[derive(Clone, Debug)]
pub struct BlocklistState {
    inner: Arc<RwLock<Arc<BlocklistSnapshot>>>,
}

impl Default for BlocklistState {
    fn default() -> Self {
        Self::empty()
    }
}

impl BlocklistState {
    /// Build an empty snapshot (no IPs, no URLs). Used by tests and by the
    /// initial boot before [`Self::refresh`] runs.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(BlocklistSnapshot::default()))),
        }
    }

    /// Build a snapshot directly from already-parsed values. Used in tests.
    #[must_use]
    pub fn from_snapshot(snapshot: BlocklistSnapshot) -> Self {
        Self {
            inner: Arc::new(RwLock::new(Arc::new(snapshot))),
        }
    }

    /// Get the current snapshot. Cheap — clones the `Arc`, drops the read
    /// guard, and hands the clone back to the caller.
    pub async fn snapshot(&self) -> Arc<BlocklistSnapshot> {
        Arc::clone(&*self.inner.read().await)
    }

    /// Replace the current snapshot with `next`.
    pub async fn install(&self, next: BlocklistSnapshot) {
        *self.inner.write().await = Arc::new(next);
    }

    /// Rebuild the snapshot by re-reading both blocklist tables.
    ///
    /// Called once on boot and from every admin mutation. The compile cost
    /// is paid on the writer side so reads never see a half-applied update.
    ///
    /// # Errors
    ///
    /// Propagates [`StorageError`] from the repository reads. A failure in
    /// `regex::RegexSet::new` (operator persisted an invalid pattern out of
    /// band) surfaces as [`StorageError::InvalidInput`].
    pub async fn refresh_from<I, U>(&self, ip: &I, url: &U) -> Result<(), StorageError>
    where
        I: IpBlocklistRepository + ?Sized,
        U: UrlBlocklistRepository + ?Sized,
    {
        let ip_rows = ip.list_all().await?;
        let url_rows = url.list_all().await?;

        let mut ip_nets = Vec::with_capacity(ip_rows.len());
        for row in &ip_rows {
            // Parse failures here should be impossible — the admin endpoint
            // validates before persisting — but a stray manual insert
            // shouldn't bring the server down. Log + skip the bad row.
            match row.cidr.parse::<IpNet>() {
                Ok(net) => ip_nets.push(net),
                Err(err) => {
                    tracing::warn!(
                        cidr = %row.cidr,
                        id = %row.id,
                        error = %err,
                        "skipping invalid CIDR in ip_blocklist"
                    );
                }
            }
        }

        // Pre-filter URL patterns the same way: compile each one individually
        // so a single bad regex doesn't poison the whole set.
        let mut url_patterns: Vec<String> = Vec::with_capacity(url_rows.len());
        for row in &url_rows {
            if regex::Regex::new(&row.pattern).is_ok() {
                url_patterns.push(row.pattern.clone());
            } else {
                tracing::warn!(
                    pattern = %row.pattern,
                    id = %row.id,
                    "skipping invalid regex in url_blocklist"
                );
            }
        }
        let url_regex_set = RegexSet::new(&url_patterns).map_err(|err| {
            StorageError::invalid_input(format!("compiling url blocklist RegexSet: {err}"))
        })?;

        self.install(BlocklistSnapshot {
            ip_nets,
            url_regex_set,
            url_patterns,
        })
        .await;
        Ok(())
    }
}

/// Parse the operator-supplied `trusted_proxies` strings into a vector of
/// `IpNet`. Used at boot (after `Config::validate` has already accepted
/// every entry) by the API state wiring.
///
/// Returns the first parse failure as an `Err` — but in practice
/// [`Config::validate`] catches this earlier, so the function is effectively
/// infallible at runtime.
///
/// # Errors
///
/// Propagates the underlying [`ipnet::AddrParseError`] as a string.
pub fn parse_trusted_proxies(strings: &[String]) -> Result<Vec<IpNet>, String> {
    strings
        .iter()
        .map(|s| {
            s.parse::<IpNet>()
                .map_err(|err| format!("invalid trusted_proxy {s:?}: {err}"))
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn snapshot_with(ips: &[&str], urls: &[&str]) -> BlocklistSnapshot {
        let ip_nets = ips.iter().map(|s| s.parse().unwrap()).collect();
        let url_patterns: Vec<String> = urls.iter().map(|s| (*s).to_string()).collect();
        let url_regex_set = RegexSet::new(&url_patterns).unwrap();
        BlocklistSnapshot {
            ip_nets,
            url_regex_set,
            url_patterns,
        }
    }

    #[test]
    fn contains_ipv4_exact() {
        let snap = snapshot_with(&["203.0.113.42/32"], &[]);
        assert!(snap.contains_ip("203.0.113.42".parse().unwrap()));
        assert!(!snap.contains_ip("203.0.113.43".parse().unwrap()));
    }

    #[test]
    fn contains_ipv4_subnet() {
        let snap = snapshot_with(&["203.0.113.0/24"], &[]);
        assert!(snap.contains_ip("203.0.113.0".parse().unwrap()));
        assert!(snap.contains_ip("203.0.113.255".parse().unwrap()));
        assert!(!snap.contains_ip("203.0.114.0".parse().unwrap()));
    }

    #[test]
    fn contains_ipv4_edges() {
        let snap = snapshot_with(&["10.0.0.0/8"], &[]);
        // /8 boundaries.
        assert!(snap.contains_ip("10.0.0.0".parse().unwrap()));
        assert!(snap.contains_ip("10.255.255.255".parse().unwrap()));
        // Just outside.
        assert!(!snap.contains_ip("9.255.255.255".parse().unwrap()));
        assert!(!snap.contains_ip("11.0.0.0".parse().unwrap()));
    }

    #[test]
    fn contains_ipv6_exact() {
        let snap = snapshot_with(&["2001:db8::1/128"], &[]);
        assert!(snap.contains_ip("2001:db8::1".parse().unwrap()));
        assert!(!snap.contains_ip("2001:db8::2".parse().unwrap()));
    }

    #[test]
    fn contains_ipv6_subnet() {
        let snap = snapshot_with(&["2001:db8::/32"], &[]);
        assert!(snap.contains_ip("2001:db8::".parse().unwrap()));
        assert!(snap.contains_ip("2001:db8:ffff:ffff:ffff:ffff:ffff:ffff".parse().unwrap()));
        assert!(!snap.contains_ip("2001:db9::".parse().unwrap()));
    }

    #[test]
    fn contains_ipv6_64() {
        let snap = snapshot_with(&["2001:db8:abcd::/64"], &[]);
        assert!(snap.contains_ip("2001:db8:abcd::1".parse().unwrap()));
        assert!(!snap.contains_ip("2001:db8:abce::1".parse().unwrap()));
    }

    #[test]
    fn ipv4_mapped_ipv6_does_not_match_plain_ipv4_cidr() {
        // ::ffff:203.0.113.42 is the IPv4-mapped IPv6 form. By design, the
        // `ipnet` crate keeps the address families separate — a v4-mapped v6
        // value is *not* a member of a plain v4 CIDR. This codifies the
        // intentional behaviour so callers that wrap an incoming v6 socket
        // peer in v4 form can decide whether to canonicalise.
        let snap = snapshot_with(&["203.0.113.0/24"], &[]);
        assert!(!snap.contains_ip("::ffff:203.0.113.42".parse().unwrap()));
        // …but a v6 CIDR over the same prefix matches:
        let snap = snapshot_with(&["::ffff:203.0.113.0/120"], &[]);
        assert!(snap.contains_ip("::ffff:203.0.113.42".parse().unwrap()));
    }

    #[test]
    fn url_word_boundary() {
        let snap = snapshot_with(&[], &[r"\bevil\.example\b"]);
        assert!(snap.matching_patterns("https://evil.example/foo").is_some());
        assert!(snap.matching_patterns("https://notevil.example/foo").is_none());
    }

    #[test]
    fn url_case_insensitivity_via_flag() {
        let snap = snapshot_with(&[], &[r"(?i)EVIL"]);
        assert!(snap.matching_patterns("https://evil.example/foo").is_some());
        assert!(snap.matching_patterns("https://EVIL.example/foo").is_some());
    }

    #[test]
    fn url_no_match_returns_none() {
        let snap = snapshot_with(&[], &[r"\.zip$"]);
        assert!(snap.matching_patterns("https://benign.example/").is_none());
    }

    #[test]
    fn parse_trusted_proxies_accepts_valid_cidrs() {
        let out = parse_trusted_proxies(&[
            "10.0.0.0/8".to_string(),
            "::1/128".to_string(),
        ])
        .unwrap();
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn parse_trusted_proxies_rejects_bad_cidr() {
        let err = parse_trusted_proxies(&["not-an-ip".to_string()]).unwrap_err();
        assert!(err.contains("not-an-ip"));
    }
}
