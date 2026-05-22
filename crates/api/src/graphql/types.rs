//! GraphQL object wrappers around the `thewiki-core` domain entities.
//!
//! The domain types in `thewiki-core` already derive `Serialize` + `ToSchema`
//! for the REST surface. We deliberately *don't* layer `async-graphql`'s
//! `SimpleObject` derive directly on those types — that would couple the core
//! crate to async-graphql and force every read field into the GraphQL surface.
//! Instead, each GraphQL object is a thin wrapper that holds the domain value
//! (or a reference to its repository handle) and exposes the surface we want
//! to ship.
//!
//! ## Pagination shape
//!
//! Connections follow a stripped-down Relay shape:
//!
//! ```graphql
//! type PageConnection {
//!   items: [Page!]!
//!   pageInfo: PageInfo!
//! }
//!
//! type PageInfo {
//!   hasNextPage: Boolean!
//!   endCursor: String
//! }
//! ```
//!
//! Edges-and-nodes is the full Relay spec but it doubles the depth of every
//! list query for no benefit on our surface (no edge-level metadata yet). The
//! `items` + `pageInfo` form mirrors what the REST surface already returns
//! (`items: [...], next_cursor: "..."`) so clients can reason about the two
//! APIs side-by-side. We can introduce an edge type later behind a feature
//! flag without breaking existing queries.

use async_graphql::{ComplexObject, Enum, ID, SimpleObject};
use serde_json::Value;
use thewiki_core::{
    AuditLogId, NamespaceId, PageId, Permissions, RevisionId, RoleId, UserId,
    page::Page as CorePage, revision::Revision as CoreRevision, role::Role as CoreRole,
    user::User as CoreUser,
};
use thewiki_storage::repo::AuditLogEntry as CoreAuditLogEntry;
use time::OffsetDateTime;

use crate::pages::revisions::{DiffHunk as RestDiffHunk, DiffLine as RestDiffLine, DiffResponse};

/// Cursor + "has next" pagination metadata, returned alongside every
/// connection's `items` array.
#[derive(SimpleObject, Debug, Clone)]
pub struct PageInfo {
    /// `true` when the underlying list has more rows beyond the current
    /// slice. When `false`, callers can stop paginating.
    pub has_next_page: bool,
    /// Opaque cursor to feed into the next call's `cursor:` argument.
    /// `None` exactly when `has_next_page = false`.
    pub end_cursor: Option<String>,
}

impl PageInfo {
    /// Build from the storage layer's `next` cursor option.
    #[must_use]
    pub fn from_next(next: Option<thewiki_storage::repo::Cursor>) -> Self {
        match next {
            Some(c) => Self {
                has_next_page: true,
                end_cursor: Some(c.0),
            },
            None => Self {
                has_next_page: false,
                end_cursor: None,
            },
        }
    }
}

/// GraphQL representation of a wiki page.
///
/// `content` is the head revision's body; `contentHtml` is its rendered
/// HTML. Both fields are zero-cost when not selected by the query — the
/// resolver only loads / renders them if the client asks.
#[derive(Debug, Clone, SimpleObject)]
#[graphql(complex)]
pub struct Page {
    /// Stable identifier.
    pub id: ID,
    /// Namespace this page lives in.
    #[graphql(name = "namespaceId")]
    pub namespace_id: ID,
    /// Slug of the namespace this page lives in (joined for display).
    #[graphql(name = "namespaceSlug")]
    pub namespace_slug: String,
    /// URL slug, unique within `namespaceSlug`.
    pub slug: String,
    /// Human-readable title.
    pub title: String,
    /// Identifier of the current head revision, if any.
    #[graphql(name = "currentRevisionId")]
    pub current_revision_id: Option<ID>,
    /// Body of the current head revision (raw, before render).
    pub content: String,
    /// Rendered, sanitised HTML for [`Self::content`].
    #[graphql(name = "contentHtml")]
    pub content_html: String,
    /// When the page row was first created.
    #[graphql(name = "createdAt")]
    pub created_at: OffsetDateTime,
    /// When the page row was last touched.
    #[graphql(name = "updatedAt")]
    pub updated_at: OffsetDateTime,
}

#[ComplexObject]
impl Page {
    /// Convenience accessor exposing the namespace ID as a typed scalar
    /// equivalent. Kept narrow so we don't multiply the surface.
    #[graphql(name = "isHeadless")]
    async fn is_headless(&self) -> bool {
        self.current_revision_id.is_none()
    }
}

impl Page {
    /// Build from the REST `PageView` payload. The REST layer already
    /// hydrates content + content_html through the renderer, so we reuse
    /// that pipeline verbatim — no duplicate render logic.
    #[must_use]
    pub fn from_view(view: crate::pages::dto::PageView) -> Self {
        Self {
            id: ID(view.id.into_uuid().to_string()),
            namespace_id: ID(view.namespace_id.into_uuid().to_string()),
            namespace_slug: view.namespace_slug,
            slug: view.slug,
            title: view.title,
            current_revision_id: view
                .current_revision_id
                .map(|r| ID(r.into_uuid().to_string())),
            content: view.content,
            content_html: view.content_html,
            created_at: view.created_at,
            updated_at: view.updated_at,
        }
    }

    /// Lightweight constructor for list endpoints — no content / html.
    /// `is_headless` will return `false` when `current_revision_id` is set.
    #[must_use]
    pub fn from_core_summary(page: CorePage, namespace_slug: String) -> Self {
        Self {
            id: ID(page.id.into_uuid().to_string()),
            namespace_id: ID(page.namespace_id.into_uuid().to_string()),
            namespace_slug,
            slug: page.slug,
            title: page.title,
            current_revision_id: page
                .current_revision_id
                .map(|r| ID(r.into_uuid().to_string())),
            content: String::new(),
            content_html: String::new(),
            created_at: page.created_at,
            updated_at: page.updated_at,
        }
    }
}

/// Relay-style connection over [`Page`].
#[derive(SimpleObject, Debug, Clone)]
pub struct PageConnection {
    /// Rows in this batch.
    pub items: Vec<Page>,
    /// Pagination metadata.
    pub page_info: PageInfo,
}

/// GraphQL representation of a [`thewiki_core::Revision`].
#[derive(Debug, Clone, SimpleObject)]
pub struct Revision {
    /// Stable identifier.
    pub id: ID,
    /// Page this revision belongs to.
    #[graphql(name = "pageId")]
    pub page_id: ID,
    /// Previous revision in this page's history, or `None` for the first.
    #[graphql(name = "parentId")]
    pub parent_id: Option<ID>,
    /// User who authored this revision.
    #[graphql(name = "authorId")]
    pub author_id: ID,
    /// Optional short note describing the edit.
    #[graphql(name = "editSummary")]
    pub edit_summary: Option<String>,
    /// Raw revision body (the page renderer's input).
    pub body: String,
    /// When the revision was committed.
    #[graphql(name = "createdAt")]
    pub created_at: OffsetDateTime,
}

impl From<CoreRevision> for Revision {
    fn from(r: CoreRevision) -> Self {
        Self {
            id: ID(r.id.into_uuid().to_string()),
            page_id: ID(r.page_id.into_uuid().to_string()),
            parent_id: r.parent_id.map(|p| ID(p.into_uuid().to_string())),
            author_id: ID(r.author_id.into_uuid().to_string()),
            edit_summary: r.edit_summary,
            body: r.body,
            created_at: r.created_at,
        }
    }
}

/// Relay-style connection over [`Revision`].
#[derive(SimpleObject, Debug, Clone)]
pub struct RevisionConnection {
    /// Rows in this batch, newest first.
    pub items: Vec<Revision>,
    /// Pagination metadata.
    pub page_info: PageInfo,
}

/// GraphQL representation of a user account.
///
/// Mirrors the REST `UserPayload` — login handle + display name + email +
/// the union of permission bits as a human-readable string.
#[derive(Debug, Clone, SimpleObject)]
pub struct User {
    /// Stable identifier.
    pub id: ID,
    /// Login handle. Case-sensitive.
    pub username: String,
    /// Display name (falls back to username at the UI layer).
    #[graphql(name = "displayName")]
    pub display_name: Option<String>,
    /// Contact email, if known.
    pub email: Option<String>,
    /// Role names this user holds. Empty when the user has no roles.
    pub roles: Vec<String>,
    /// Effective permission flags as `"READ | EDIT | CREATE"`. The empty
    /// string represents no permissions (a registered user with no roles).
    pub permissions: String,
    /// When the account was created.
    #[graphql(name = "createdAt")]
    pub created_at: OffsetDateTime,
    /// When the user last successfully logged in, or `None` for never.
    #[graphql(name = "lastLoginAt")]
    pub last_login_at: Option<OffsetDateTime>,
}

impl User {
    /// Build a [`User`] from the core type plus the user's role set.
    #[must_use]
    pub fn from_parts(user: CoreUser, roles: &[CoreRole], permissions: Permissions) -> Self {
        Self {
            id: ID(user.id.into_uuid().to_string()),
            username: user.username.as_str().to_owned(),
            display_name: user.display_name,
            email: user.email.map(|e| e.into_string()),
            roles: roles.iter().map(|r| r.name.as_str().to_owned()).collect(),
            permissions: format_permissions(permissions),
            created_at: user.created_at,
            last_login_at: user.last_login_at,
        }
    }
}

fn format_permissions(p: Permissions) -> String {
    use bitflags::Flags;
    let mut parts = Vec::new();
    for flag in Permissions::FLAGS {
        if p.contains(*flag.value()) {
            parts.push(flag.name());
        }
    }
    parts.join(" | ")
}

/// GraphQL representation of a role.
#[derive(Debug, Clone, SimpleObject)]
pub struct Role {
    /// Stable identifier.
    pub id: ID,
    /// Machine-friendly identifier.
    pub name: String,
    /// Human-readable label.
    #[graphql(name = "displayName")]
    pub display_name: String,
    /// Effective permission flags as `"READ | EDIT | CREATE"`.
    pub permissions: String,
}

impl From<CoreRole> for Role {
    fn from(r: CoreRole) -> Self {
        Self {
            id: ID(r.id.into_uuid().to_string()),
            name: r.name.as_str().to_owned(),
            display_name: r.display_name,
            permissions: format_permissions(r.permissions),
        }
    }
}

/// GraphQL representation of a namespace.
#[derive(Debug, Clone, SimpleObject)]
pub struct Namespace {
    /// Stable identifier.
    pub id: ID,
    /// URL-safe slug.
    pub slug: String,
    /// Human-readable label.
    #[graphql(name = "displayName")]
    pub display_name: String,
}

impl From<thewiki_core::Namespace> for Namespace {
    fn from(n: thewiki_core::Namespace) -> Self {
        Self {
            id: ID(n.id.into_uuid().to_string()),
            slug: n.slug.into_string(),
            display_name: n.display_name,
        }
    }
}

/// One entry in the recent-changes feed.
///
/// Mirrors the REST `RecentChangeView` exactly — only the field naming
/// follows GraphQL's camelCase convention.
#[derive(Debug, Clone, SimpleObject)]
pub struct RecentChange {
    /// Identifier of the revision this entry refers to.
    #[graphql(name = "revisionId")]
    pub revision_id: ID,
    /// The page that was edited.
    #[graphql(name = "pageId")]
    pub page_id: ID,
    /// URL slug of the edited page.
    #[graphql(name = "pageSlug")]
    pub page_slug: String,
    /// Namespace the edited page lives in.
    #[graphql(name = "namespaceId")]
    pub namespace_id: ID,
    /// Slug of the namespace.
    #[graphql(name = "namespaceSlug")]
    pub namespace_slug: String,
    /// User who committed the revision.
    #[graphql(name = "authorId")]
    pub author_id: ID,
    /// Username of the author.
    #[graphql(name = "authorUsername")]
    pub author_username: String,
    /// Optional short note describing the edit.
    #[graphql(name = "editSummary")]
    pub edit_summary: Option<String>,
    /// When the revision was committed.
    #[graphql(name = "createdAt")]
    pub created_at: OffsetDateTime,
}

impl From<thewiki_storage::repo::RecentChange> for RecentChange {
    fn from(rc: thewiki_storage::repo::RecentChange) -> Self {
        Self {
            revision_id: ID(rc.revision_id.into_uuid().to_string()),
            page_id: ID(rc.page_id.into_uuid().to_string()),
            page_slug: rc.page_slug,
            namespace_id: ID(rc.namespace_id.into_uuid().to_string()),
            namespace_slug: rc.namespace_slug,
            author_id: ID(rc.author_id.into_uuid().to_string()),
            author_username: rc.author_username,
            edit_summary: rc.edit_summary,
            created_at: rc.created_at,
        }
    }
}

/// Relay-style connection over [`RecentChange`].
#[derive(SimpleObject, Debug, Clone)]
pub struct RecentChangeConnection {
    /// Rows in this batch, newest first.
    pub items: Vec<RecentChange>,
    /// Pagination metadata.
    pub page_info: PageInfo,
}

/// Kind of line in a [`DiffHunk`].
#[derive(Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// Line identical in both sides — provided as surrounding context.
    Context,
    /// Line added in the `to` revision.
    Insertion,
    /// Line removed from the `from` revision.
    Deletion,
}

impl From<crate::pages::revisions::DiffKind> for DiffKind {
    fn from(k: crate::pages::revisions::DiffKind) -> Self {
        // The REST `DiffKind` is `#[non_exhaustive]` but only `Context`,
        // `Insertion`, and `Deletion` are defined today. A wildcard arm
        // would unlock graceful forward-compat with future variants, but
        // the compiler (correctly) sees it as unreachable today — when a
        // new variant lands, the build fails here, which is the desired
        // signal to come back and decide the mapping deliberately.
        match k {
            crate::pages::revisions::DiffKind::Context => Self::Context,
            crate::pages::revisions::DiffKind::Insertion => Self::Insertion,
            crate::pages::revisions::DiffKind::Deletion => Self::Deletion,
        }
    }
}

/// A single line in a [`DiffHunk`].
#[derive(Debug, Clone, SimpleObject)]
pub struct DiffLine {
    /// Whether the line is context / insertion / deletion.
    pub kind: DiffKind,
    /// Content of the line (with trailing newline as in the source).
    pub content: String,
}

impl From<RestDiffLine> for DiffLine {
    fn from(l: RestDiffLine) -> Self {
        Self {
            kind: l.kind.into(),
            content: l.content,
        }
    }
}

/// A unified-diff hunk: a contiguous span of changed lines + context.
#[derive(Debug, Clone, SimpleObject)]
pub struct DiffHunk {
    /// 1-based line number of the first line on the `from` side, or `0`
    /// when the hunk represents a pure insertion.
    #[graphql(name = "oldStart")]
    pub old_start: u32,
    /// Number of lines from the `from` side covered by this hunk.
    #[graphql(name = "oldCount")]
    pub old_count: u32,
    /// 1-based line number of the first line on the `to` side, or `0` when
    /// the hunk represents a pure deletion.
    #[graphql(name = "newStart")]
    pub new_start: u32,
    /// Number of lines from the `to` side covered by this hunk.
    #[graphql(name = "newCount")]
    pub new_count: u32,
    /// Lines in the hunk, in display order.
    pub lines: Vec<DiffLine>,
}

impl From<RestDiffHunk> for DiffHunk {
    fn from(h: RestDiffHunk) -> Self {
        Self {
            old_start: h.old_start,
            old_count: h.old_count,
            new_start: h.new_start,
            new_count: h.new_count,
            lines: h.lines.into_iter().map(Into::into).collect(),
        }
    }
}

/// Pairwise revision diff (mirrors the REST `DiffResponse`).
#[derive(Debug, Clone, SimpleObject)]
pub struct Diff {
    /// Revision the diff was computed from.
    pub from: ID,
    /// Revision the diff was computed to.
    pub to: ID,
    /// Ready-to-display unified-diff text.
    pub unified: String,
    /// Same diff, broken into structured hunks for callers that render
    /// their own side-by-side view.
    pub hunks: Vec<DiffHunk>,
}

impl From<DiffResponse> for Diff {
    fn from(d: DiffResponse) -> Self {
        Self {
            from: ID(d.from.into_uuid().to_string()),
            to: ID(d.to.into_uuid().to_string()),
            unified: d.unified,
            hunks: d.hunks.into_iter().map(Into::into).collect(),
        }
    }
}

/// One hit in a search result set.
#[derive(Debug, Clone, SimpleObject)]
pub struct SearchHit {
    /// Page primary key.
    #[graphql(name = "pageId")]
    pub page_id: ID,
    /// Namespace slug of the page.
    #[graphql(name = "namespaceSlug")]
    pub namespace_slug: String,
    /// URL slug of the page.
    pub slug: String,
    /// Page title.
    pub title: String,
    /// HTML snippet with matched terms wrapped in `<mark>…</mark>`.
    pub snippet: String,
    /// BM25-derived relevance score (higher is better).
    pub score: f64,
    /// Last-edited timestamp of the matched page.
    #[graphql(name = "updatedAt")]
    pub updated_at: Option<OffsetDateTime>,
}

impl From<thewiki_search::SearchHit> for SearchHit {
    fn from(h: thewiki_search::SearchHit) -> Self {
        Self {
            page_id: ID(h.page_id.into_uuid().to_string()),
            namespace_slug: h.namespace_slug,
            slug: h.slug,
            title: h.title,
            snippet: h.snippet,
            score: f64::from(h.score),
            updated_at: h.updated_at,
        }
    }
}

/// Result set for a search query.
#[derive(Debug, Clone, SimpleObject)]
pub struct SearchResults {
    /// Ranked hits, descending by score.
    pub hits: Vec<SearchHit>,
    /// Best-effort estimate of the total matching document count.
    #[graphql(name = "totalEstimate")]
    pub total_estimate: u64,
}

impl From<thewiki_search::SearchResults> for SearchResults {
    fn from(r: thewiki_search::SearchResults) -> Self {
        Self {
            hits: r.hits.into_iter().map(Into::into).collect(),
            total_estimate: r.total_estimate,
        }
    }
}

/// One row of the administrative audit log.
#[derive(Debug, Clone, SimpleObject)]
pub struct AuditLogEntry {
    /// Audit entry id.
    pub id: ID,
    /// Actor user id.
    #[graphql(name = "actorId")]
    pub actor_id: ID,
    /// Actor username snapshot.
    #[graphql(name = "actorUsername")]
    pub actor_username: String,
    /// Stable machine action, e.g. `page.create`.
    pub action: String,
    /// Target kind (`page`, `user`, ...).
    #[graphql(name = "targetKind")]
    pub target_kind: String,
    /// Target identifier (as string — may not be one of our domain IDs).
    #[graphql(name = "targetId")]
    pub target_id: ID,
    /// Human-readable target label at event time.
    #[graphql(name = "targetLabel")]
    pub target_label: Option<String>,
    /// Structured metadata, encoded as a JSON string. We return JSON rather
    /// than a typed object because the metadata shape varies by action and
    /// shipping a per-action union would multiply the surface for marginal
    /// gain — clients typically format this for display only.
    #[graphql(name = "metadataJson")]
    pub metadata_json: String,
    /// Event timestamp.
    #[graphql(name = "createdAt")]
    pub created_at: OffsetDateTime,
}

impl From<CoreAuditLogEntry> for AuditLogEntry {
    fn from(e: CoreAuditLogEntry) -> Self {
        Self {
            id: ID(e.id.into_uuid().to_string()),
            actor_id: ID(e.actor_id.into_uuid().to_string()),
            actor_username: e.actor_username,
            action: e.action,
            target_kind: e.target_kind,
            target_id: ID(e.target_id.to_string()),
            target_label: e.target_label,
            metadata_json: serialize_metadata(&e.metadata),
            created_at: e.created_at,
        }
    }
}

fn serialize_metadata(value: &Value) -> String {
    // `Value::to_string` is infallible — it serialises the in-memory tree
    // directly. We never expect to fail here.
    value.to_string()
}

/// Relay-style connection over [`AuditLogEntry`].
#[derive(SimpleObject, Debug, Clone)]
pub struct AuditLogConnection {
    /// Rows in this batch, newest first.
    pub items: Vec<AuditLogEntry>,
    /// Pagination metadata.
    pub page_info: PageInfo,
}

/// Returned by the `login` mutation on success.
///
/// Mirrors REST's `UserPayload` — we don't expose the cookie value because
/// the server already sets it via `Set-Cookie` headers on the response.
#[derive(Debug, Clone, SimpleObject)]
pub struct LoginPayload {
    /// The freshly-authenticated user.
    pub user: User,
}

/// Helper: turn a typed `UserId` into a GraphQL `ID`. Used by resolvers
/// that build a wrapper without first round-tripping through the core
/// type's `Display` impl.
#[must_use]
pub fn user_id_to_gql(id: UserId) -> ID {
    ID(id.into_uuid().to_string())
}

/// Helper: turn a typed `PageId` into a GraphQL `ID`.
#[must_use]
pub fn page_id_to_gql(id: PageId) -> ID {
    ID(id.into_uuid().to_string())
}

/// Helper: turn a typed `RevisionId` into a GraphQL `ID`.
#[must_use]
pub fn revision_id_to_gql(id: RevisionId) -> ID {
    ID(id.into_uuid().to_string())
}

/// Helper: turn a typed `NamespaceId` into a GraphQL `ID`.
#[must_use]
pub fn namespace_id_to_gql(id: NamespaceId) -> ID {
    ID(id.into_uuid().to_string())
}

/// Helper: turn a typed `RoleId` into a GraphQL `ID`.
#[must_use]
pub fn role_id_to_gql(id: RoleId) -> ID {
    ID(id.into_uuid().to_string())
}

/// Helper: turn a typed `AuditLogId` into a GraphQL `ID`.
#[must_use]
pub fn audit_id_to_gql(id: AuditLogId) -> ID {
    ID(id.into_uuid().to_string())
}
