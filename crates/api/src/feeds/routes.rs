//! Atom feed handlers (#46).
//!
//! See the [`super`] module docs for the design rationale; this file is just
//! the handlers + the (small) helpers that render a [`Feed`] from each row
//! shape.

use atom_syndication::{Entry, Feed, FixedDateTime, LinkBuilder, Person, PersonBuilder, Text};
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::str::FromStr;
use thewiki_core::{NamespaceSlug, PageId, ProtectionLevel, RevisionId};
use thewiki_storage::repo::{
    NamespaceRepository, RecentChange, RecentChangesFilter, RecentChangesRepository,
    RevisionRepository, UserRepository, WatchRepository, WatchedPage,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::auth::AuthSession;
use crate::error::ApiError;
use crate::feeds::FEED_LIMIT;
use crate::state::{AppState, AppStorage};

/// `GET /api/v1/recent-changes.atom` — wiki-wide Atom feed.
#[utoipa::path(
    get,
    path = "/recent-changes.atom",
    responses(
        (status = 200, description = "Atom feed", content_type = "application/atom+xml", body = String),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "recent-changes",
)]
pub async fn recent_changes_atom<S: AppStorage>(
    State(state): State<AppState<S>>,
) -> Result<Response, ApiError> {
    let slice = state
        .storage
        .recent_changes()
        .list(RecentChangesFilter::default(), None, FEED_LIMIT)
        .await?;
    Ok(render_recent_changes_feed(
        &slice.items,
        "thewiki recent changes",
        "/api/v1/recent-changes.atom",
        "urn:thewiki:recent-changes",
    ))
}

/// `GET /api/v1/recent-changes/{namespace}/atom` — per-namespace Atom feed.
///
/// Axum's `matchit`-based router rejects parameters that share a path
/// segment with literal text (`{namespace}.atom` would error), so the
/// namespace feed lives under a `/atom` sub-segment. The wiki-wide feed at
/// `/recent-changes.atom` is unaffected because its full path is a literal.
#[utoipa::path(
    get,
    path = "/recent-changes/{namespace}/atom",
    params(("namespace" = String, Path, description = "Namespace slug")),
    responses(
        (status = 200, description = "Atom feed", content_type = "application/atom+xml", body = String),
        (status = 400, description = "Invalid namespace slug", body = crate::error::ErrorBody),
        (status = 404, description = "Namespace not found", body = crate::error::ErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    tag = "recent-changes",
)]
pub async fn recent_changes_namespace_atom<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(namespace): Path<String>,
) -> Result<Response, ApiError> {
    let slug = NamespaceSlug::new(&namespace)
        .map_err(|err| ApiError::InvalidInput(format!("namespace: {err}")))?;
    let ns = state.storage.namespaces().get_by_slug(&slug).await?;
    let filter = RecentChangesFilter {
        namespace_id: Some(ns.id),
        ..RecentChangesFilter::default()
    };
    let slice = state
        .storage
        .recent_changes()
        .list(filter, None, FEED_LIMIT)
        .await?;
    Ok(render_recent_changes_feed(
        &slice.items,
        &format!("thewiki recent changes — {}", ns.slug.as_str()),
        &format!("/api/v1/recent-changes/{}/atom", ns.slug.as_str()),
        &format!("urn:thewiki:recent-changes:{}", ns.slug.as_str()),
    ))
}

/// `GET /api/v1/watchlist.atom` — Atom feed of the caller's watched pages.
///
/// Each entry corresponds to a watched page's latest revision so feed
/// readers behave the same way they do on the wiki-wide route: a fresh
/// `<updated>` whenever someone edits a page the user watches.
#[utoipa::path(
    get,
    path = "/watchlist.atom",
    responses(
        (status = 200, description = "Atom feed", content_type = "application/atom+xml", body = String),
        (status = 401, description = "Missing or expired session", body = crate::auth::error::AuthErrorBody),
        (status = 429, description = "Rate limit exceeded", body = crate::rate_limit::RateLimitErrorBody),
    ),
    security(("SessionCookie" = [])),
    tag = "watchlist",
)]
pub async fn watchlist_atom<S: AppStorage>(
    State(state): State<AppState<S>>,
    session: AuthSession,
) -> Result<Response, ApiError> {
    let watched = state
        .storage
        .watches()
        .list_for_user(session.user.id, FEED_LIMIT)
        .await?;

    // For each watched page we fetch the latest revision so the entry can
    // carry the author + edit summary + revision UUID. A page that has no
    // revisions yet (shouldn't happen in practice; pages are created with an
    // initial revision) falls back to the page-level timestamps. The author
    // username comes from a per-entry user lookup; the watchlist is bounded
    // at FEED_LIMIT, so N+1 here is at most 50 round-trips — fine for an
    // out-of-band feed read, and keeps the storage trait shape narrow.
    let mut entries: Vec<Entry> = Vec::with_capacity(watched.len());
    for page in &watched {
        let entry = match state.storage.revisions().head_of(page.page_id).await {
            Ok(rev) => {
                let author_name = state
                    .storage
                    .users()
                    .get_by_id(rev.author_id)
                    .await
                    .map(|u| u.username.as_str().to_owned())
                    .unwrap_or_else(|_| rev.author_id.into_uuid().to_string());
                watchlist_entry_from_revision(page, &rev, &author_name)
            }
            Err(_) => watchlist_entry_from_page(page),
        };
        entries.push(entry);
    }

    let updated = watched
        .first()
        .map(|p| p.updated_at)
        .unwrap_or_else(OffsetDateTime::now_utc);

    let feed = Feed {
        title: Text::plain(format!(
            "thewiki watchlist — {}",
            session.user.username.as_str()
        )),
        id: format!("urn:thewiki:watchlist:{}", session.user.id.into_uuid()),
        updated: to_fixed(updated),
        links: vec![self_link("/api/v1/watchlist.atom")],
        entries,
        ..Default::default()
    };
    Ok(finalise_feed(feed))
}

// ─── Renderers ───────────────────────────────────────────────────────────────

/// Build the recent-changes Atom feed and wrap it in a 200 response.
fn render_recent_changes_feed(
    items: &[RecentChange],
    title: &str,
    self_href: &str,
    feed_id: &str,
) -> Response {
    let public_items: Vec<&RecentChange> = items.iter().filter(|rc| is_public(rc)).collect();
    let updated = public_items
        .first()
        .map(|rc| rc.created_at)
        .unwrap_or_else(OffsetDateTime::now_utc);

    let entries = public_items
        .iter()
        .map(|rc| recent_change_entry(rc))
        .collect();

    let feed = Feed {
        title: Text::plain(title.to_owned()),
        id: feed_id.to_owned(),
        updated: to_fixed(updated),
        links: vec![self_link(self_href)],
        entries,
        ..Default::default()
    };

    finalise_feed(feed)
}

/// Predicate: should a `RecentChange` row be exposed on the public feeds?
///
/// Pages at `None` or `SemiProtected` are publicly viewable; anything
/// stronger is treated as restricted for syndication purposes even though
/// the protection model is edit-side today. This is the conservative
/// reading of "feeds respect protection".
fn is_public(rc: &RecentChange) -> bool {
    matches!(
        rc.protection_level,
        ProtectionLevel::None | ProtectionLevel::SemiProtected
    )
}

/// Build one `<entry>` for the recent-changes feeds.
fn recent_change_entry(rc: &RecentChange) -> Entry {
    let title = format!("{}:{}", rc.namespace_slug, rc.page_slug);
    let summary = rc
        .edit_summary
        .clone()
        .unwrap_or_else(|| "(no summary)".to_owned());
    Entry {
        id: tag_uri(&rc.page_id, &rc.revision_id),
        title: Text::plain(title),
        updated: to_fixed(rc.created_at),
        authors: vec![author(&rc.author_username)],
        summary: Some(Text::plain(summary)),
        links: vec![page_link(&rc.namespace_slug, &rc.page_slug)],
        ..Default::default()
    }
}

/// Build a watchlist entry from the page plus its newest revision.
fn watchlist_entry_from_revision(
    page: &WatchedPage,
    rev: &thewiki_core::Revision,
    author_name: &str,
) -> Entry {
    let title = format!("{}:{}", page.namespace_slug, page.page_slug);
    let summary = rev
        .edit_summary
        .clone()
        .unwrap_or_else(|| "(no summary)".to_owned());
    Entry {
        id: tag_uri(&page.page_id, &rev.id),
        title: Text::plain(title),
        updated: to_fixed(rev.created_at),
        authors: vec![author(author_name)],
        summary: Some(Text::plain(summary)),
        links: vec![page_link(&page.namespace_slug, &page.page_slug)],
        ..Default::default()
    }
}

/// Fallback watchlist entry — used only if `head_of` couldn't resolve the
/// page's most recent revision (a defensive branch; pages are always created
/// with an initial revision).
fn watchlist_entry_from_page(page: &WatchedPage) -> Entry {
    let title = format!("{}:{}", page.namespace_slug, page.page_slug);
    Entry {
        id: format!("tag:thewiki,2026:page/{}", page.page_id.into_uuid()),
        title: Text::plain(title),
        updated: to_fixed(page.updated_at),
        summary: Some(Text::plain(page.page_title.clone())),
        links: vec![page_link(&page.namespace_slug, &page.page_slug)],
        ..Default::default()
    }
}

/// Render the [`Feed`] to XML and wrap it in a 200 response with the proper
/// `Content-Type`.
fn finalise_feed(feed: Feed) -> Response {
    let xml = feed.to_string();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/atom+xml; charset=utf-8")],
        xml,
    )
        .into_response()
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// `<link rel="self" type="application/atom+xml" href="...">`.
fn self_link(href: &str) -> atom_syndication::Link {
    LinkBuilder::default()
        .href(href.to_owned())
        .rel("self".to_owned())
        .mime_type(Some("application/atom+xml".to_owned()))
        .build()
}

/// `<link rel="alternate" type="text/html" href="/wiki/{ns}/{slug}">`.
fn page_link(namespace_slug: &str, page_slug: &str) -> atom_syndication::Link {
    LinkBuilder::default()
        .href(format!("/wiki/{}/{}", namespace_slug, page_slug))
        .rel("alternate".to_owned())
        .mime_type(Some("text/html".to_owned()))
        .build()
}

/// `<author><name>…</name></author>`.
fn author(name: &str) -> Person {
    PersonBuilder::default().name(name.to_owned()).build()
}

/// Build a `tag:` URI from the page and revision UUIDs.
///
/// The tag scheme (RFC 4151) gives us a stable, decentralised identifier
/// without relying on the wiki's external hostname. Feed readers dedupe by
/// `<id>` so it has to stay stable across publishes.
fn tag_uri(page_id: &PageId, revision_id: &RevisionId) -> String {
    format!(
        "tag:thewiki,2026:page/{}/revision/{}",
        page_id.into_uuid(),
        revision_id.into_uuid()
    )
}

/// Convert an [`OffsetDateTime`] into the [`atom_syndication`] timestamp
/// type.
///
/// `FixedDateTime` is `chrono::DateTime<FixedOffset>`. We round-trip via the
/// RFC 3339 string form so we don't have to take chrono as a direct
/// dependency — both crates agree on the wire shape.
fn to_fixed(ts: OffsetDateTime) -> FixedDateTime {
    let s = ts
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned());
    FixedDateTime::from_str(&s).unwrap_or_else(|_| {
        #[allow(clippy::expect_used, reason = "epoch parses to a known-good FixedDateTime")]
        FixedDateTime::from_str("1970-01-01T00:00:00Z")
            .expect("epoch RFC3339 always parses as a fixed datetime")
    })
}
