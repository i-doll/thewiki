//! Atom 1.0 syndication feeds (#46).
//!
//! Three routes share a single Atom renderer:
//!
//! * `GET /api/v1/recent-changes.atom` — wiki-wide chronological edit feed.
//! * `GET /api/v1/recent-changes/{namespace}/atom` — per-namespace edit
//!   feed. (Axum's `matchit` router doesn't allow `{namespace}.atom`
//!   because of the literal-after-param suffix, so the namespace feed
//!   lives one segment deeper. The wiki-wide and watchlist feeds keep the
//!   `.atom` suffix because their full paths are literals.)
//! * `GET /api/v1/watchlist.atom` — the calling user's watched pages,
//!   keyed by the page's latest revision.
//!
//! Each route is capped at [`FEED_LIMIT`] entries and emits
//! `application/atom+xml; charset=utf-8` regardless of `Accept`. The body is
//! produced by the [`atom_syndication`] crate so the XML conforms to RFC 4287
//! out of the box (matching the issue's "feed validates" acceptance
//! criterion).
//!
//! ## Protection filtering
//!
//! The two recent-changes routes drop any row whose `protection_level` is
//! stronger than [`SemiProtected`]. The protection model is edit-side today
//! ([`ProtectionLevel`] currently gates writes, not reads), but the issue's
//! "feeds respect protection (private pages not exposed)" line is the
//! relevant intent: anything that signals "this page is not for general
//! consumption" should not appear on an anonymous Atom feed. The watchlist
//! feed, in contrast, is scoped to one authenticated user; the user has
//! already chosen to subscribe so every watched page is included.
//!
//! [`ProtectionLevel`]: thewiki_core::ProtectionLevel
//! [`SemiProtected`]: thewiki_core::ProtectionLevel::SemiProtected

pub mod routes;

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::state::{AppState, AppStorage};

/// Hard cap on entries per feed. The issue specifies 50; we don't expose a
/// `?limit=` knob because Atom readers walk the feed periodically and the
/// number is small.
pub const FEED_LIMIT: u32 = 50;

/// Build the Atom feed subrouter wrapped in a utoipa [`OpenApiRouter`].
///
/// Mounted by [`crate::app::build_with_state`] and
/// [`crate::app::build_full`] directly under `/api/v1` — each path already
/// carries its full prefix (`/recent-changes.atom`, …) so callers don't have
/// to nest twice.
pub fn router<S: AppStorage>() -> OpenApiRouter<AppState<S>> {
    OpenApiRouter::new()
        .routes(routes!(routes::recent_changes_atom))
        .routes(routes!(routes::recent_changes_namespace_atom))
        .routes(routes!(routes::watchlist_atom))
}
