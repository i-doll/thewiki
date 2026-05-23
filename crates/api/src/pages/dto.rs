//! Request and response payloads for the page CRUD endpoints.
//!
//! Every type here derives [`Serialize`]/[`Deserialize`] for the wire form and
//! [`ToSchema`] so the OpenAPI surface picks it up automatically. The shapes
//! are intentionally narrower than the domain entities — `PageView` for
//! instance flattens the namespace slug onto the response so clients don't
//! need a second round trip just to render a breadcrumb.

use serde::{Deserialize, Serialize};
use thewiki_core::{CategoryId, NamespaceId, PageId, PendingRevisionId, ProtectionLevel, RevisionId};
use time::OffsetDateTime;
use utoipa::ToSchema;

/// Body of `POST /api/v1/pages` and `POST /api/v1/wiki/{namespace}`.
///
/// For the namespace-aware route (`/api/v1/wiki/{namespace}`, added in #28)
/// the namespace is taken from the URL and `namespace_slug` is ignored if
/// provided. For the legacy `/api/v1/pages` route, `namespace_slug` is
/// optional and defaults to `Main`.
use crate::categories::dto::CategoryView;

/// Body of `POST /api/v1/pages`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct CreatePageRequest {
    /// Slug of the namespace this page lives in. Optional — the namespace
    /// can also be carried in the URL path (`/api/v1/wiki/{namespace}`) or
    /// defaulted to `Main` on the legacy `/api/v1/pages` route. The
    /// namespace must already exist; the API does not create namespaces on
    /// demand.
    #[serde(default)]
    pub namespace_slug: Option<String>,
    /// URL-safe slug, unique within the resolved namespace.
    pub slug: String,
    /// Human-readable title shown in the UI.
    pub title: String,
    /// Initial body for the page. The first revision is committed with this
    /// content.
    pub content: String,
    /// Optional category ids to assign on create. Omit / empty for no
    /// categorisation.
    #[serde(default)]
    pub categories: Option<Vec<CategoryId>>,
    /// Optional flat tag list. Strings are validated and lowercased on the
    /// server; rejected with `400` if any tag fails validation.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// Body of `PUT /api/v1/pages/{slug}`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct UpdatePageRequest {
    /// New title. Omitting it keeps the existing title.
    #[serde(default)]
    pub title: Option<String>,
    /// New body. Always required — an update commits a new revision.
    pub content: String,
    /// Optional short note describing the edit (think Git commit message).
    #[serde(default)]
    pub edit_summary: Option<String>,
    /// New category set. When present the entire set is replaced
    /// (so passing `[]` clears every assignment). Omit to leave the
    /// existing set untouched.
    #[serde(default)]
    pub categories: Option<Vec<CategoryId>>,
    /// New tag set. Same replace-on-present semantics as `categories`.
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

/// Hypermedia-style link block surfaced alongside a [`PageView`] (#43).
///
/// Today the only field is `talk`, which the SPA uses to render the
/// "Discuss" sidebar link. Adding new links here is additive — existing
/// clients that ignore the `_links` block are unaffected.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct PageLinks {
    /// URL of the discussion ("talk") page for this page, when the
    /// namespace is paired with a `Talk_*` companion. `None` for pages
    /// already in a talk namespace (no "talk of a talk").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub talk: Option<String>,
}

/// Sign-with-timestamp convention metadata (#43).
///
/// Mirrors the server-side `~~~~` expansion so the SPA can preview the
/// rendered signature client-side without a round-trip. The marker is
/// MediaWiki-compatible — operators familiar with `mw:Help:Signatures`
/// won't have to retrain editors.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SignatureConvention {
    /// Marker that triggers signature expansion. Always `"~~~~"`.
    pub marker: String,
    /// Format string describing the expansion. `{user}` is replaced with
    /// `[[User:<username>]]`; `{timestamp}` with an ISO-8601 UTC timestamp.
    pub format: String,
}

impl Default for SignatureConvention {
    fn default() -> Self {
        Self {
            marker: "~~~~".to_owned(),
            format: "[[User:{user}]] {timestamp}".to_owned(),
        }
    }
}

/// A single page returned by the read endpoints.
///
/// `content` is the body of the current revision (joined in for
/// convenience); `content_html` is the same body rendered through
/// [`thewiki_render::MarkdownRenderer`] with all `[[WikiLink]]`s resolved
/// against the page repository (so missing targets are styled as redlinks)
/// and the result sanitised by ammonia. The HTML is safe to embed.
/// `content_html` was added in #30 — before that the API returned only the
/// raw body and the SPA rendered it client-side.
///
/// Listing endpoints use the lighter [`PageListItem`] instead so they don't
/// have to ship every page's full body.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PageView {
    /// Stable identifier.
    pub id: PageId,
    /// Namespace this page lives in.
    pub namespace_id: NamespaceId,
    /// Slug of the namespace; joined in so clients don't need a second
    /// round trip just to render a breadcrumb.
    pub namespace_slug: String,
    /// URL slug, unique within the namespace.
    pub slug: String,
    /// Human-readable title.
    pub title: String,
    /// Pointer to the current head revision. `None` only in the transient
    /// state between page creation and the first revision being committed
    /// (today that window is closed inside `POST /api/v1/pages`).
    pub current_revision_id: Option<RevisionId>,
    /// Body of the current revision, or empty string if no revision exists.
    pub content: String,
    /// Rendered, sanitised HTML of [`Self::content`].
    ///
    /// Empty when `content` is empty. Wikilinks are resolved against the
    /// page repository: missing targets render with `class="redlink"` and
    /// a URL pointing at the create form (`/wiki/.../edit?new=1`).
    pub content_html: String,
    /// How protected this page is from edits. Drives the SPA's lock badge
    /// and is enforced server-side on every mutating handler (#34).
    pub protection_level: ProtectionLevel,
    /// Categories this page is assigned to (#29).
    pub categories: Vec<CategoryView>,
    /// Flat tag set this page carries (#29). Always lowercased.
    pub tags: Vec<String>,
    /// When the page row was first created.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// When the page row was last touched.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Set to `true` on a `202 Accepted` response when the submitted edit
    /// landed in the moderation queue (#40) instead of going live. The page
    /// fields above still describe the **previous** state (or, on a fresh
    /// create, the empty placeholder page that's now reserving the slug).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub queued: bool,
    /// Identifier of the `pending_revisions` row, present only when
    /// [`Self::queued`] is `true`. Reviewers act on this id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_revision_id: Option<PendingRevisionId>,
    /// Position of the queued edit in the FIFO pending queue at the moment
    /// the row was written (1 = next to be reviewed). Present only when
    /// [`Self::queued`] is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1)]
    pub queue_position: Option<u64>,
    /// `true` if this page lives in a discussion ("talk") namespace (#43).
    /// The SPA uses this to switch the body renderer to
    /// `<TalkThread>` and to hide the "Discuss" sidebar link (talk pages
    /// don't have their own talk page).
    #[serde(default)]
    pub is_talk: bool,
    /// Hypermedia-style cross-references for this page. New surface added
    /// in #43 — additive, so existing clients that ignore it are unaffected.
    #[serde(default, rename = "_links")]
    pub links: PageLinks,
    /// Documentation of the signature-expansion convention (#43). Surfaced
    /// on every read response so the SPA can render a `~~~~` preview
    /// client-side. Independent of the page's namespace — the marker is
    /// only *expanded* on save when the page lives in a talk namespace.
    #[serde(default)]
    pub signature_convention: SignatureConvention,
}

/// Lighter representation of a page used inside [`PageListResponse`].
///
/// Lacks `content` and `namespace_id`; clients listing a namespace already
/// know the namespace.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PageListItem {
    /// Stable identifier.
    pub id: PageId,
    /// Slug of the namespace this page lives in.
    pub namespace_slug: String,
    /// URL slug.
    pub slug: String,
    /// Human-readable title.
    pub title: String,
    /// When the page row was last touched.
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// Response from `GET /api/v1/pages?cursor=…&limit=…`.
///
/// `next_cursor` is `None` once the listing has been exhausted; otherwise
/// pass it back as `?cursor=…` to fetch the next page.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PageListResponse {
    /// Rows in this batch, ordered `(created_at ASC, id ASC)` per the
    /// storage layer's contract.
    pub items: Vec<PageListItem>,
    /// Token to fetch the next page, or `None` if there are no more pages.
    pub next_cursor: Option<String>,
}

/// Query parameters for `GET /api/v1/pages`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListPagesQuery {
    /// Namespace slug to list pages from. Defaults to `Main` if absent.
    /// Namespace prefix routing lands with #28.
    #[serde(default)]
    pub namespace: Option<String>,
    /// Opaque cursor returned by a previous call. Omit to start from the
    /// beginning.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size. Clamped to
    /// [`thewiki_storage::repo::MAX_PAGE_SIZE`]. `0`/missing falls back to
    /// the route-level default.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// A single inbound link surfaced by
/// `GET /api/v1/pages/{slug}/backlinks` (#30).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BacklinkItem {
    /// Stable identifier of the source page.
    pub page_id: PageId,
    /// Namespace slug the source page lives in.
    pub namespace_slug: String,
    /// URL slug of the source page.
    pub page_slug: String,
    /// Human-readable title of the source page.
    pub title: String,
}

/// Response from `GET /api/v1/pages/{slug}/backlinks` (#30).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BacklinkListResponse {
    /// Pages that link to the queried target, ordered `(source_page_id ASC)`.
    pub items: Vec<BacklinkItem>,
    /// Token to fetch the next page, or `None` if the listing is exhausted.
    pub next_cursor: Option<String>,
}

/// Query parameters for `GET /api/v1/pages/{slug}/backlinks`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListBacklinksQuery {
    /// Opaque cursor returned by a previous call. Omit to start from the
    /// beginning.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size. Clamped to
    /// [`thewiki_storage::repo::MAX_PAGE_SIZE`]. `0`/missing falls back to
    /// the route-level default.
    #[serde(default)]
    pub limit: Option<u32>,
}
