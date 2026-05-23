//! IP and URL blocklist subsystem (#42).
//!
//! The blocklist runs ahead of auth: every request resolves its perceived
//! client IP (the socket peer, unless [`SecurityConfig::trust_x_forwarded_for`]
//! is set and the peer is in [`SecurityConfig::trusted_proxies`]) and we
//! match the IP against an in-memory `Vec<IpNet>` snapshot. The URL list is
//! consulted from the page create / update handlers — every URL extracted
//! from the submitted Markdown body is checked against a compiled
//! `regex::RegexSet`.
//!
//! Persistence lives behind [`IpBlocklistRepository`] /
//! [`UrlBlocklistRepository`]; the API state holds a [`BlocklistState`] that
//! owns the in-memory snapshot. On boot the state is hydrated from storage;
//! every admin mutation refreshes it via `tokio::sync::RwLock` so reads stay
//! lock-free in the common case.
//!
//! Submodules:
//! - [`state`] — [`BlocklistState`] and the load / refresh logic.
//! - [`peer_ip`] — extract the effective client IP from a request.
//! - [`middleware`] — Axum layer that returns 403 for blocklisted IPs.
//! - [`url_check`] — extract URLs from Markdown bodies and match them.
//!
//! [`SecurityConfig`]: crate::config::SecurityConfig
//! [`IpBlocklistRepository`]: thewiki_storage::repo::IpBlocklistRepository
//! [`UrlBlocklistRepository`]: thewiki_storage::repo::UrlBlocklistRepository

pub mod middleware;
pub mod peer_ip;
pub mod state;
pub mod url_check;

pub use middleware::{BlocklistedError, BlocklistedErrorBody, blocklist_layer};
pub use peer_ip::effective_client_ip;
pub use state::{BlocklistSnapshot, BlocklistState, parse_trusted_proxies};
pub use url_check::{UrlBlockMatch, extract_urls};
