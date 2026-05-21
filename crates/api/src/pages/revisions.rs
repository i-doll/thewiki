//! Revision listing and diff endpoints.
//!
//! Builds on the page CRUD routes in [`super::routes`]. Two endpoints live
//! here:
//!
//! * `GET /api/v1/pages/{slug}/revisions` — paginated history.
//! * `GET /api/v1/pages/{slug}/diff?from=…&to=…` — pairwise comparison
//!   between two revisions of the same page. The response carries both a
//!   ready-to-display unified-diff string and structured hunks so the SPA
//!   doesn't have to re-parse the unified text to render a side-by-side view.
//!
//! Both handlers are generic over the storage facade (`S: AppStorage`) so the
//! route layer stays backend-agnostic.

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use thewiki_core::{NamespaceSlug, PageId, RevisionId, UserId};
use thewiki_storage::repo::{Cursor, NamespaceRepository, PageRepository, RevisionRepository};
use time::OffsetDateTime;
use utoipa::ToSchema;

use crate::error::ApiError;
use crate::state::{AppState, AppStorage};

/// Default namespace slug used when a request doesn't carry one.
///
/// Mirrors the constant in [`super::routes`]; namespace prefix routing lands
/// with #28.
const DEFAULT_NAMESPACE: &str = "Main";

/// Maximum character count we include in a [`RevisionView::body_excerpt`].
///
/// List endpoints don't ship the full body — clients hit the diff endpoint
/// (or a future `/revisions/{id}` route) when they need the entire snapshot.
const BODY_EXCERPT_CHARS: usize = 200;

/// Compact summary of a revision used by the list endpoint.
///
/// `body_excerpt` is the first ~200 characters of the revision body. It's a
/// rendered string (not bytes), counted by `chars`, so multi-byte runes are
/// not split halfway through.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RevisionView {
    /// Stable identifier.
    pub id: RevisionId,
    /// Page this revision belongs to.
    pub page_id: PageId,
    /// Previous revision in the page's history, or `None` for the first
    /// revision.
    pub parent_id: Option<RevisionId>,
    /// User who authored this revision.
    pub author_id: UserId,
    /// Optional short note describing the edit (think Git commit message).
    pub edit_summary: Option<String>,
    /// First ~200 characters of the body. Use the diff endpoint to compare
    /// the full body against another revision.
    pub body_excerpt: String,
    /// When the revision was committed.
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl RevisionView {
    /// Build a list-friendly view of a [`thewiki_core::Revision`], truncating
    /// the body to [`BODY_EXCERPT_CHARS`] without splitting a UTF-8 scalar.
    fn from_revision(revision: thewiki_core::Revision) -> Self {
        let body_excerpt = if revision.body.chars().count() <= BODY_EXCERPT_CHARS {
            revision.body
        } else {
            revision.body.chars().take(BODY_EXCERPT_CHARS).collect()
        };
        Self {
            id: revision.id,
            page_id: revision.page_id,
            parent_id: revision.parent_id,
            author_id: revision.author_id,
            edit_summary: revision.edit_summary,
            body_excerpt,
            created_at: revision.created_at,
        }
    }
}

/// Response from `GET /api/v1/pages/{slug}/revisions`.
///
/// Items are newest-first per the storage contract. `next_cursor` is `None`
/// once the history has been fully walked; otherwise pass it back as
/// `?cursor=…` to fetch the next page.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RevisionListResponse {
    /// Revisions in this batch, newest first.
    pub items: Vec<RevisionView>,
    /// Token to fetch the next page, or `None` if there are no more
    /// revisions.
    pub next_cursor: Option<String>,
}

/// Query parameters for `GET /api/v1/pages/{slug}/revisions`.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
pub struct ListRevisionsQuery {
    /// Opaque cursor returned by a previous call. Omit to start from the
    /// newest revision.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Page size. Clamped to
    /// [`thewiki_storage::repo::MAX_PAGE_SIZE`]. `0`/missing falls back to
    /// the route-level default.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Query parameters for `GET /api/v1/pages/{slug}/diff`.
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
pub struct DiffQuery {
    /// Revision the diff is computed *from* — shown as `-` lines.
    pub from: RevisionId,
    /// Revision the diff is computed *to* — shown as `+` lines.
    pub to: RevisionId,
}

/// Kind of line in a [`DiffHunk`].
///
/// `#[non_exhaustive]` so we can add new variants (e.g. `ChangeInPlace`,
/// `NoNewlineAtEof`) without breaking downstream matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DiffKind {
    /// Line is identical in both sides — provided as surrounding context.
    Context,
    /// Line was added in the `to` revision.
    Insertion,
    /// Line was removed from the `from` revision.
    Deletion,
}

/// A single line inside a [`DiffHunk`].
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiffLine {
    /// Whether the line is context / insertion / deletion.
    pub kind: DiffKind,
    /// Content of the line. Includes the trailing newline as it appears in
    /// the source so callers can reassemble a faithful unified diff.
    pub content: String,
}

/// A unified-diff hunk: a contiguous span of changed lines plus a small
/// window of context on either side.
///
/// `old_start` / `new_start` are 1-based line numbers, matching the
/// `@@ -old_start,old_count +new_start,new_count @@` convention used by
/// `git diff`. When `*_count == 0` the corresponding `*_start` is `0` (this
/// is what `git diff` does for pure additions or deletions).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiffHunk {
    /// 1-based line number of the first line in the `from` body, or `0`
    /// when the hunk represents a pure insertion.
    pub old_start: u32,
    /// Number of lines from the `from` side covered by this hunk.
    pub old_count: u32,
    /// 1-based line number of the first line in the `to` body, or `0` when
    /// the hunk represents a pure deletion.
    pub new_start: u32,
    /// Number of lines from the `to` side covered by this hunk.
    pub new_count: u32,
    /// Lines in the hunk, in display order.
    pub lines: Vec<DiffLine>,
}

/// Response from `GET /api/v1/pages/{slug}/diff`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DiffResponse {
    /// Revision the diff was computed from.
    pub from: RevisionId,
    /// Revision the diff was computed to.
    pub to: RevisionId,
    /// Ready-to-display unified-diff text. The `---`/`+++` header lines name
    /// the two revisions by id for traceability.
    pub unified: String,
    /// Same diff, broken into structured hunks for clients that want to
    /// render their own side-by-side view.
    pub hunks: Vec<DiffHunk>,
}

/// Parse a caller-supplied namespace slug, falling back to
/// [`DEFAULT_NAMESPACE`].
fn parse_default_namespace_slug() -> Result<NamespaceSlug, ApiError> {
    NamespaceSlug::new(DEFAULT_NAMESPACE)
        .map_err(|err| ApiError::InvalidInput(format!("namespace_slug: {err}")))
}

/// Resolve a page by URL slug in the default namespace.
async fn resolve_page<S: AppStorage>(
    state: &AppState<S>,
    slug: &str,
) -> Result<thewiki_core::Page, ApiError> {
    let namespace_slug = parse_default_namespace_slug()?;
    let namespace = state
        .storage
        .namespaces()
        .get_by_slug(&namespace_slug)
        .await?;
    let page = state
        .storage
        .pages()
        .get_by_namespace_and_slug(namespace.id, slug)
        .await?;
    Ok(page)
}

/// `GET /api/v1/pages/{slug}/revisions` — paginated history for a page.
///
/// Returns 404 if the page itself does not exist. A page that exists but has
/// no revisions yet (a transient state — see the doc on
/// `pages.current_revision_id`) returns an empty list.
#[utoipa::path(
    get,
    path = "/{slug}/revisions",
    params(
        ("slug" = String, Path, description = "URL slug within the default namespace"),
        ListRevisionsQuery,
    ),
    responses(
        (status = 200, description = "Revision history", body = RevisionListResponse),
        (status = 404, description = "Page not found", body = crate::error::ErrorBody),
    ),
    tag = "revisions",
)]
pub async fn list_revisions<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(slug): Path<String>,
    Query(query): Query<ListRevisionsQuery>,
) -> Result<Json<RevisionListResponse>, ApiError> {
    let page = resolve_page(&state, &slug).await?;

    let limit = match query.limit {
        Some(0) | None => state.route_config.default_page_size,
        Some(n) => n,
    };
    let cursor = query.cursor.map(Cursor);

    let slice = state
        .storage
        .revisions()
        .list_for_page(page.id, cursor, limit)
        .await?;

    let items = slice
        .items
        .into_iter()
        .map(RevisionView::from_revision)
        .collect();
    Ok(Json(RevisionListResponse {
        items,
        next_cursor: slice.next.map(|c| c.0),
    }))
}

/// `GET /api/v1/pages/{slug}/diff?from=<rev>&to=<rev>` — pairwise revision
/// diff.
///
/// Both `from` and `to` must belong to the page identified by `{slug}`; a
/// mismatch (or an unknown revision id) yields 404 — we do not want to leak
/// the existence of revisions on other pages.
#[utoipa::path(
    get,
    path = "/{slug}/diff",
    params(
        ("slug" = String, Path, description = "URL slug within the default namespace"),
        DiffQuery,
    ),
    responses(
        (status = 200, description = "Diff between two revisions", body = DiffResponse),
        (status = 404, description = "Page or revision not found", body = crate::error::ErrorBody),
    ),
    tag = "revisions",
)]
pub async fn diff_revisions<S: AppStorage>(
    State(state): State<AppState<S>>,
    Path(slug): Path<String>,
    Query(query): Query<DiffQuery>,
) -> Result<Json<DiffResponse>, ApiError> {
    let page = resolve_page(&state, &slug).await?;

    let from = state.storage.revisions().get_by_id(query.from).await?;
    let to = state.storage.revisions().get_by_id(query.to).await?;
    // Both revisions must belong to this page. Map a mismatch to 404 so we
    // don't reveal that the id exists elsewhere.
    if from.page_id != page.id || to.page_id != page.id {
        return Err(ApiError::NotFound);
    }

    let diff = build_diff(query.from, query.to, &from.body, &to.body);
    Ok(Json(diff))
}

/// Build a [`DiffResponse`] from two revision bodies.
///
/// Pulled out of the handler for ease of testing — no I/O, no async.
fn build_diff(
    from_id: RevisionId,
    to_id: RevisionId,
    from_body: &str,
    to_body: &str,
) -> DiffResponse {
    let diff = TextDiff::from_lines(from_body, to_body);

    // Unified-diff text. The `---`/`+++` header lines name the revisions by
    // id so a copy-pasted diff carries enough metadata to be useful in bug
    // reports.
    let unified = diff
        .unified_diff()
        .context_radius(3)
        .header(&from_id.to_string(), &to_id.to_string())
        .to_string();

    // Structured hunks. We re-walk the diff because `unified_diff()` consumes
    // the builder by value when serialised; cheap given `TextDiff` itself
    // caches the LCS internally.
    let mut hunks = Vec::new();
    for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
        let ops = hunk.ops();
        if ops.is_empty() {
            continue;
        }

        // `as_tag_tuple()` returns 0-based half-open ranges. Convert to the
        // 1-based `@@ -old_start,old_count +new_start,new_count @@`
        // convention git uses; `*_start = 0` when the corresponding count
        // is 0 (pure insertion / deletion).
        #[allow(
            clippy::expect_used,
            reason = "ops is guarded above for non-empty; first/last cannot be None"
        )]
        let first = ops.first().expect("non-empty ops");
        #[allow(
            clippy::expect_used,
            reason = "ops is guarded above for non-empty; first/last cannot be None"
        )]
        let last = ops.last().expect("non-empty ops");
        let (_, old_first_range, new_first_range) = first.as_tag_tuple();
        let (_, old_last_range, new_last_range) = last.as_tag_tuple();

        let old_count = (old_last_range.end - old_first_range.start) as u32;
        let new_count = (new_last_range.end - new_first_range.start) as u32;
        let old_start = if old_count == 0 {
            0
        } else {
            (old_first_range.start as u32) + 1
        };
        let new_start = if new_count == 0 {
            0
        } else {
            (new_first_range.start as u32) + 1
        };

        let mut lines = Vec::new();
        for op in ops {
            for change in diff.iter_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => DiffKind::Context,
                    ChangeTag::Delete => DiffKind::Deletion,
                    ChangeTag::Insert => DiffKind::Insertion,
                };
                lines.push(DiffLine {
                    kind,
                    content: change.value().to_string(),
                });
            }
        }

        hunks.push(DiffHunk {
            old_start,
            old_count,
            new_start,
            new_count,
            lines,
        });
    }

    DiffResponse {
        from: from_id,
        to: to_id,
        unified,
        hunks,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn build_diff_basic_change() {
        let from_id = RevisionId::new();
        let to_id = RevisionId::new();
        let diff = build_diff(
            from_id,
            to_id,
            "alpha\nbeta\ngamma\n",
            "alpha\nBETA\ngamma\n",
        );
        assert!(diff.unified.contains("-beta"));
        assert!(diff.unified.contains("+BETA"));
        assert_eq!(diff.hunks.len(), 1);
        let hunk = &diff.hunks[0];
        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.new_start, 1);
        // Three context lines on each side of the change collapse to the
        // whole 3-line buffer since the file is short.
        assert_eq!(hunk.old_count, 3);
        assert_eq!(hunk.new_count, 3);
        let deletions = hunk
            .lines
            .iter()
            .filter(|l| l.kind == DiffKind::Deletion)
            .count();
        let insertions = hunk
            .lines
            .iter()
            .filter(|l| l.kind == DiffKind::Insertion)
            .count();
        assert_eq!(deletions, 1);
        assert_eq!(insertions, 1);
    }

    #[test]
    fn build_diff_inverts_when_args_swapped() {
        let from_id = RevisionId::new();
        let to_id = RevisionId::new();
        let forward = build_diff(from_id, to_id, "a\nb\n", "a\nB\n");
        let reverse = build_diff(to_id, from_id, "a\nB\n", "a\nb\n");
        let forward_dels = forward
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == DiffKind::Deletion)
            .count();
        let reverse_ins = reverse
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == DiffKind::Insertion)
            .count();
        assert_eq!(forward_dels, reverse_ins);
    }

    #[test]
    fn build_diff_identical_bodies_has_no_hunks() {
        let from_id = RevisionId::new();
        let to_id = RevisionId::new();
        let diff = build_diff(from_id, to_id, "same\n", "same\n");
        assert!(diff.hunks.is_empty());
    }

    #[test]
    fn revision_view_truncates_long_bodies() {
        use thewiki_core::{PageId, Revision, UserId};
        let long_body: String = "x".repeat(BODY_EXCERPT_CHARS * 2);
        let rev = Revision::new(PageId::new(), None, UserId::new(), long_body, None);
        let view = RevisionView::from_revision(rev);
        assert_eq!(view.body_excerpt.chars().count(), BODY_EXCERPT_CHARS);
    }
}
