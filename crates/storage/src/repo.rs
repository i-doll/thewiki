//! Persistence traits — one per domain aggregate.
//!
//! The shape of every trait is the same: async methods returning
//! [`Result<T, StorageError>`](crate::StorageError). Backends implement these;
//! the API crate consumes them generically (`fn handler<R: PageRepository>`)
//! and stays backend-agnostic at the call site.
//!
//! ## A note on `async fn` in trait and `dyn`-compatibility
//!
//! These traits use native `async fn` (stable on Rust 1.92). That keeps the
//! definitions clean, but `async_fn_in_trait` is **not** object-safe in stable
//! Rust today — `Box<dyn PageRepository>` or `Arc<dyn PageRepository>` will
//! not compile. The intentional path is therefore monomorphisation via
//! generics, which the API layer adopts when wiring handlers.
//!
//! If a future revision needs trait objects (e.g. a runtime-pluggable
//! storage backend), we will introduce a `Send`-bounded variant via
//! [`trait_variant`](https://crates.io/crates/trait-variant) (`#[trait_variant::make(SendXxx: Send)]`)
//! or rewrite the return position as `impl Future<Output = …> + Send + '_`.
//! Both are mechanical changes; the current decision is to stay minimal until
//! the API integration in #9 forces our hand.
//!
//! Pagination is **cursor-based** — list endpoints accept a [`Cursor`] and a
//! `limit`, and return a slice plus an opaque next-cursor token. Offset
//! pagination is intentionally avoided so large tables don't degrade and so
//! the wire form stays stable across sorted-by changes.

use std::time::Duration;

use serde_json::Value;
use thewiki_core::{
    AuditLogId, Category, CategoryId, Media, MediaId, Namespace, NamespaceId, NamespaceSlug, Page,
    PageId, ProtectionLevel, Revision, RevisionId, Role, RoleId, RoleName, Session, SessionId, Tag,
    User, UserId, Username,
};
use time::OffsetDateTime;

use crate::error::StorageError;

/// Opaque pagination cursor.
///
/// Backends choose the encoding (typically `(created_at, id)` or just `id`).
/// Callers receive a cursor from a `list_*` call and pass it back to fetch
/// the next page. `None` means "start from the beginning".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor(pub String);

impl Cursor {
    /// Borrow the inner token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A page of results plus the cursor that fetches the next page.
///
/// `next` is `None` once the listing has been exhausted. The name avoids
/// `Page`, which is the wiki-page domain entity.
#[derive(Debug, Clone)]
pub struct PageSlice<T> {
    /// Rows in this batch.
    pub items: Vec<T>,
    /// Cursor to pass to the next call, or `None` if no more rows remain.
    pub next: Option<Cursor>,
}

/// Default page size when a caller passes `0`.
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// Hard upper bound on per-call rows. Backends enforce this so a malicious
/// caller can't trigger an unbounded `SELECT`.
pub const MAX_PAGE_SIZE: u32 = 500;

/// Persistence operations for the [`Page`] aggregate.
pub trait PageRepository: Send + Sync {
    /// Insert a page row.
    ///
    /// # Errors
    ///
    /// * [`StorageError::Conflict`] if `(namespace_id, slug)` is already taken.
    /// * [`StorageError::Database`] if the namespace foreign key doesn't
    ///   resolve, or on any lower-level driver failure.
    fn create(&self, page: &Page) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Fetch a page by primary key.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if no row matches `id`. Other failures
    /// propagate as [`StorageError::Database`].
    fn get_by_id(&self, id: PageId) -> impl Future<Output = Result<Page, StorageError>> + Send;

    /// Resolve a page by its `(namespace_id, slug)` URL form.
    ///
    /// # Errors
    ///
    /// As [`get_by_id`](Self::get_by_id).
    fn get_by_namespace_and_slug(
        &self,
        namespace_id: NamespaceId,
        slug: &str,
    ) -> impl Future<Output = Result<Page, StorageError>> + Send;

    /// List pages in a namespace, cursor paginated.
    ///
    /// Order is `(created_at ASC, id ASC)` — stable and aligned with the
    /// UUIDv7 prefix so the index walk stays sequential.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidInput`] if `cursor` is malformed for this
    /// backend.
    fn list_in_namespace(
        &self,
        namespace_id: NamespaceId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> impl Future<Output = Result<PageSlice<Page>, StorageError>> + Send;

    /// Update a page row in place.
    ///
    /// All mutable columns (`title`, `slug`, `current_revision_id`,
    /// `protection_level`, `content_format`, `updated_at`) are overwritten
    /// from `page`. `id`, `namespace_id`, and `created_at` are immutable
    /// once written and any change is silently ignored.
    ///
    /// # Errors
    ///
    /// * [`StorageError::NotFound`] if no row with this `id` exists.
    /// * [`StorageError::Conflict`] if the rename collides with another page
    ///   in the same namespace.
    fn update(&self, page: &Page) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Delete a page (and its revisions, via `ON DELETE CASCADE`).
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if the row didn't exist.
    fn delete(&self, id: PageId) -> impl Future<Output = Result<(), StorageError>> + Send;
}

/// Persistence operations for the [`Revision`] aggregate.
pub trait RevisionRepository: Send + Sync {
    /// Append a revision.
    ///
    /// # Errors
    ///
    /// * [`StorageError::Database`] if `page_id` doesn't reference an existing
    ///   page (FK violation).
    fn create(&self, revision: &Revision) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Fetch a revision by primary key.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if no row matches.
    fn get_by_id(
        &self,
        id: RevisionId,
    ) -> impl Future<Output = Result<Revision, StorageError>> + Send;

    /// List revisions for `page_id` newest first, cursor paginated.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidInput`] if `cursor` is malformed.
    fn list_for_page(
        &self,
        page_id: PageId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> impl Future<Output = Result<PageSlice<Revision>, StorageError>> + Send;

    /// Return the newest revision of `page_id`.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if the page has no revisions yet (this is
    /// distinct from the page not existing — the caller should distinguish if
    /// it cares).
    fn head_of(
        &self,
        page_id: PageId,
    ) -> impl Future<Output = Result<Revision, StorageError>> + Send;
}

/// Persistence operations for the [`User`] aggregate.
pub trait UserRepository: Send + Sync {
    /// Insert a user row. The optional `password_hash` is stored opaque.
    ///
    /// # Errors
    ///
    /// [`StorageError::Conflict`] if `username` is already taken.
    fn create(
        &self,
        user: &User,
        password_hash: Option<&str>,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Fetch a user by primary key.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if no row matches.
    fn get_by_id(&self, id: UserId) -> impl Future<Output = Result<User, StorageError>> + Send;

    /// Resolve a user by login handle.
    ///
    /// # Errors
    ///
    /// As [`get_by_id`](Self::get_by_id).
    fn get_by_username(
        &self,
        username: &Username,
    ) -> impl Future<Output = Result<User, StorageError>> + Send;

    /// Update mutable user columns (`email`, `display_name`, `last_login_at`).
    /// `username`, `id`, and `created_at` are immutable.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if the user no longer exists.
    fn update(&self, user: &User) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Delete a user. Cascade-deletes their role assignments.
    ///
    /// # Errors
    ///
    /// * [`StorageError::NotFound`] if the row didn't exist.
    /// * [`StorageError::Conflict`] if the FK from `revisions.author_id` would
    ///   be violated (we use `ON DELETE RESTRICT`).
    fn delete(&self, id: UserId) -> impl Future<Output = Result<(), StorageError>> + Send;
}

/// Persistence operations for the [`Namespace`] aggregate.
pub trait NamespaceRepository: Send + Sync {
    /// Insert a namespace.
    ///
    /// # Errors
    ///
    /// [`StorageError::Conflict`] if `slug` is already in use.
    fn create(
        &self,
        namespace: &Namespace,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Fetch a namespace by primary key.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if no row matches.
    fn get_by_id(
        &self,
        id: NamespaceId,
    ) -> impl Future<Output = Result<Namespace, StorageError>> + Send;

    /// Resolve a namespace by its URL slug.
    ///
    /// # Errors
    ///
    /// As [`get_by_id`](Self::get_by_id).
    fn get_by_slug(
        &self,
        slug: &NamespaceSlug,
    ) -> impl Future<Output = Result<Namespace, StorageError>> + Send;

    /// List every namespace. The set is small enough that pagination is
    /// unnecessary — operators rarely define more than a handful.
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures as
    /// [`StorageError::Database`].
    fn list(&self) -> impl Future<Output = Result<Vec<Namespace>, StorageError>> + Send;

    /// Update only the human-readable display name of a namespace.
    ///
    /// Slug renames are intentionally not supported here — they would
    /// invalidate URLs and have cascading effects across the link graph,
    /// search index, and audit log. Renaming the display name is safe.
    ///
    /// # Errors
    ///
    /// * [`StorageError::NotFound`] if no namespace has this `id`.
    fn update_display_name(
        &self,
        id: NamespaceId,
        display_name: &str,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Delete a namespace.
    ///
    /// The caller is expected to verify the namespace contains no pages
    /// before invoking this — the schema's FK from `pages.namespace_id` is
    /// `ON DELETE RESTRICT`, so a non-empty namespace produces a
    /// [`StorageError::Conflict`].
    ///
    /// # Errors
    ///
    /// * [`StorageError::NotFound`] if the row didn't exist.
    /// * [`StorageError::Conflict`] if pages still reference the namespace.
    fn delete(&self, id: NamespaceId) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Ensure the default `Main` namespace exists, returning it whether it
    /// was just created or already present. Idempotent across racing
    /// callers — typically invoked once at server boot.
    ///
    /// The default uses the `Main` slug and `"Main"` display name. Operators
    /// can rename the display name afterwards via [`update_display_name`];
    /// the slug stays fixed because URL routing (#28) treats `Main` as the
    /// implicit prefix.
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures as
    /// [`StorageError::Database`].
    fn get_or_create_default(&self)
    -> impl Future<Output = Result<Namespace, StorageError>> + Send;
}

/// Persistence operations for the [`Role`] aggregate.
pub trait RoleRepository: Send + Sync {
    /// Insert a role.
    ///
    /// # Errors
    ///
    /// [`StorageError::Conflict`] if `name` is already in use.
    fn create(&self, role: &Role) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Fetch a role by primary key.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if no row matches.
    fn get_by_id(&self, id: RoleId) -> impl Future<Output = Result<Role, StorageError>> + Send;

    /// Resolve a role by its machine name.
    ///
    /// # Errors
    ///
    /// As [`get_by_id`](Self::get_by_id).
    fn get_by_name(
        &self,
        name: &RoleName,
    ) -> impl Future<Output = Result<Role, StorageError>> + Send;

    /// List every defined role. As with namespaces this is a small set and
    /// pagination would be ceremonial.
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures.
    fn list(&self) -> impl Future<Output = Result<Vec<Role>, StorageError>> + Send;

    /// Grant `role_id` to `user_id`. Idempotent — assigning a role a user
    /// already holds is a no-op.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] if either FK doesn't resolve.
    fn assign_to_user(
        &self,
        user_id: UserId,
        role_id: RoleId,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Revoke `role_id` from `user_id`. Idempotent — revoking a role the user
    /// doesn't hold is a no-op.
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures.
    fn revoke_from_user(
        &self,
        user_id: UserId,
        role_id: RoleId,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Enumerate the roles `user_id` holds.
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures.
    fn list_for_user(
        &self,
        user_id: UserId,
    ) -> impl Future<Output = Result<Vec<Role>, StorageError>> + Send;
}

/// Persistence operations for the [`Session`] aggregate.
///
/// Sessions are the server-side record of an authenticated login (#13). The
/// trait is intentionally small — the auth layer owns issuance / cookie
/// formatting; storage just persists rows.
pub trait SessionRepository: Send + Sync {
    /// Create a new session that expires in `ttl` from now.
    ///
    /// Returns the freshly-minted [`Session`] (so callers don't need a
    /// follow-up `get_by_id`).
    ///
    /// # Errors
    ///
    /// * [`StorageError::Database`] if the user FK doesn't resolve or any
    ///   lower-level driver failure occurs.
    fn create(
        &self,
        user_id: UserId,
        ttl: Duration,
        user_agent: Option<&str>,
        ip_address: Option<&str>,
    ) -> impl Future<Output = Result<Session, StorageError>> + Send;

    /// Fetch a session by primary key.
    ///
    /// **Expired sessions are reported as [`StorageError::NotFound`]**: callers
    /// must not have to enforce the TTL themselves. The expired row is left in
    /// place for [`prune_expired`](Self::prune_expired) to garbage-collect.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if no row matches `id` *or* if the row has
    /// expired. Other failures propagate as [`StorageError::Database`].
    fn get_by_id(
        &self,
        id: SessionId,
    ) -> impl Future<Output = Result<Session, StorageError>> + Send;

    /// Update `last_seen_at` to "now". Called per authenticated request.
    ///
    /// Does not bump `expires_at` — TTL is fixed at issuance. If a row is
    /// already expired, this still updates `last_seen_at`; the caller should
    /// have rejected the request before reaching here.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if the row no longer exists.
    fn touch(&self, id: SessionId) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Delete a single session (logout).
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if the row didn't exist.
    fn delete(&self, id: SessionId) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Delete every session belonging to `user_id` (e.g. on password change or
    /// account disable). Returns the number of rows removed.
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures.
    fn delete_for_user(
        &self,
        user_id: UserId,
    ) -> impl Future<Output = Result<u64, StorageError>> + Send;

    /// Remove every expired session row. Returns the number of rows pruned.
    ///
    /// Cheap because the `expires_at` column is indexed.
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures.
    fn prune_expired(&self) -> impl Future<Output = Result<u64, StorageError>> + Send;
}

/// A flattened row in the recent-changes feed.
///
/// Each row stands on its own — `(page_slug, namespace_slug, author_username)`
/// are joined in so a client can render the feed without follow-up lookups.
/// Constructed by the [`RecentChangesRepository`] from a single JOIN query
/// over `revisions`, `pages`, `namespaces`, and `users`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentChange {
    /// Identifier of the [`Revision`] row this entry refers to.
    pub revision_id: RevisionId,
    /// The page that was edited.
    pub page_id: PageId,
    /// URL slug of the edited page, joined in for convenience.
    pub page_slug: String,
    /// Namespace the edited page lives in.
    pub namespace_id: NamespaceId,
    /// Slug of the namespace, joined in for convenience.
    pub namespace_slug: String,
    /// User who committed the revision.
    pub author_id: UserId,
    /// Username of the author, joined in for convenience.
    pub author_username: String,
    /// Optional short note describing the edit.
    pub edit_summary: Option<String>,
    /// When the revision was committed.
    pub created_at: OffsetDateTime,
    /// `pages.protection_level` snapshot. Carried so the Atom feed handler
    /// (#46) can skip non-public rows without a follow-up lookup.
    pub protection_level: ProtectionLevel,
}

/// Filter passed to [`RecentChangesRepository::list`].
///
/// All fields are optional — a default filter selects every revision in the
/// database (subject to the cursor and limit).
#[derive(Debug, Clone, Default)]
pub struct RecentChangesFilter {
    /// Only include revisions committed at or after this timestamp.
    pub since: Option<OffsetDateTime>,
    /// Only include revisions for pages in this namespace.
    pub namespace_id: Option<NamespaceId>,
    /// Only include revisions committed by this user.
    pub actor_id: Option<UserId>,
    /// When `true`, restrict the result set to revisions of pages whose
    /// `protection_level` is publicly viewable (`None` or `SemiProtected`).
    /// Pushed down to SQL so the `LIMIT` is applied to public rows only —
    /// otherwise an unprotected feed could under-fill when recent protected
    /// edits dominate the head of the timeline. See #46.
    pub public_only: bool,
}

/// Persistence operations for the wiki-wide recent-changes feed.
///
/// Unlike the per-aggregate repositories, this trait answers questions that
/// span multiple tables in a single read — primarily "what changed across the
/// wiki, newest first". The backend implementation is a JOIN over
/// `revisions × pages × namespaces × users` so the API can hand back a fully
/// hydrated row without N+1 lookups.
pub trait RecentChangesRepository: Send + Sync {
    /// List recent changes, newest first, cursor paginated.
    ///
    /// Order is `(created_at DESC, id DESC)`. The cursor encodes the last
    /// `(created_at, id)` pair returned and the next call resumes strictly
    /// older than it — so paginating doesn't skip or duplicate when new edits
    /// land mid-iteration.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidInput`] if `cursor` is malformed for this
    /// backend. Lower-level failures propagate as [`StorageError::Database`].
    fn list(
        &self,
        filter: RecentChangesFilter,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> impl Future<Output = Result<PageSlice<RecentChange>, StorageError>> + Send;
}

/// A persistent administrative audit-log row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditLogEntry {
    /// Primary key for this audit row.
    pub id: AuditLogId,
    /// Actor snapshot.
    pub actor_id: UserId,
    /// Actor username snapshot, retained even if the user is later renamed.
    pub actor_username: String,
    /// Stable machine action, e.g. `page.create`.
    pub action: String,
    /// Target kind, e.g. `page` or `user`.
    pub target_kind: String,
    /// Target identifier. Stored without FK so audit rows survive deletion.
    pub target_id: uuid::Uuid,
    /// Human-readable target label at event time.
    pub target_label: Option<String>,
    /// JSON metadata. Keep this small and never include page bodies or secrets.
    pub metadata: Value,
    /// Event timestamp.
    pub created_at: OffsetDateTime,
}

/// Input for inserting an audit-log row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAuditLogEntry {
    /// Actor snapshot.
    pub actor_id: UserId,
    /// Actor username snapshot.
    pub actor_username: String,
    /// Stable machine action.
    pub action: String,
    /// Target kind.
    pub target_kind: String,
    /// Target identifier.
    pub target_id: uuid::Uuid,
    /// Human-readable target label.
    pub target_label: Option<String>,
    /// JSON metadata.
    pub metadata: Value,
}

/// Page mutation paired with a required audit-log write.
///
/// Backends should commit the mutation and audit row atomically so callers do
/// not report a successful privileged action without a durable audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PageAuditMutation {
    /// Insert a new page. When `live_revision` is present, also append that
    /// revision and promote the page to it.
    CreatePage {
        /// Page row to store. For live revisions its `current_revision_id`
        /// must match `live_revision.id`; for queued edits it must be `None`.
        page: Page,
        /// Initial revision, present only when the edit publishes live.
        live_revision: Option<Revision>,
    },
    /// Append a revision to an existing page and promote the page to it.
    CommitRevision {
        /// Page row after promotion. Its `current_revision_id` must match
        /// `revision.id`.
        page: Page,
        /// Revision to append.
        revision: Revision,
    },
    /// Update mutable columns on an existing page without committing a new
    /// revision (e.g. a protection-level change, #34). Backends apply the
    /// full set of mutable columns from `page`; the caller is responsible
    /// for bumping `updated_at` before passing the row in.
    UpdatePage {
        /// Page row with the desired final state. Identity columns are
        /// immutable; everything else is overwritten.
        page: Page,
    },
    /// Delete an existing page.
    DeletePage {
        /// Page ID to delete.
        page_id: PageId,
    },
    /// Only write the audit row. Used for queued edits where no page row is
    /// mutated yet.
    AuditOnly,
}

/// Filter passed to [`AuditLogRepository::list`].
#[derive(Debug, Clone, Default)]
pub struct AuditLogFilter {
    /// Only include entries for this actor username.
    pub actor_username: Option<String>,
    /// Only include this action.
    pub action: Option<String>,
    /// Only include entries at or after this timestamp.
    pub since: Option<OffsetDateTime>,
    /// Only include entries at or before this timestamp.
    pub until: Option<OffsetDateTime>,
}

/// Persistence operations for the administrative audit log.
pub trait AuditLogRepository: Send + Sync {
    /// Insert an audit row and return the stored entry.
    ///
    /// # Errors
    ///
    /// Lower-level failures propagate as [`StorageError::Database`].
    fn create(
        &self,
        entry: NewAuditLogEntry,
    ) -> impl Future<Output = Result<AuditLogEntry, StorageError>> + Send;

    /// List audit rows, newest first, cursor paginated.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidInput`] if `cursor` is malformed for this
    /// backend. Lower-level failures propagate as [`StorageError::Database`].
    fn list(
        &self,
        filter: AuditLogFilter,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> impl Future<Output = Result<PageSlice<AuditLogEntry>, StorageError>> + Send;

    /// Delete rows older than `cutoff`. Returns the number of rows removed.
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures.
    fn prune_before(
        &self,
        cutoff: OffsetDateTime,
    ) -> impl Future<Output = Result<u64, StorageError>> + Send;
}

/// Clamp a caller-supplied `limit` to the configured page-size bounds.
#[must_use]
pub fn clamp_limit(limit: u32) -> u32 {
    let l = if limit == 0 { DEFAULT_PAGE_SIZE } else { limit };
    l.min(MAX_PAGE_SIZE)
}

/// A row in the wikilink graph: "page X links to (namespace, slug)".
///
/// Populated by the API layer's create/update path from
/// `MarkdownRenderer::extract_links`. The pair `(target_namespace_slug,
/// target_page_slug)` is stored rather than a page ID so dangling references
/// (links to not-yet-created pages, i.e. redlinks) survive and so target
/// deletion doesn't immediately scrub history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageLink {
    /// Source page that contains the `[[Target]]`.
    pub source_page_id: PageId,
    /// Namespace slug of the link target.
    pub target_namespace_slug: String,
    /// Page slug of the link target.
    pub target_page_slug: String,
}

/// A backlink row enriched with the source page's metadata for direct
/// rendering by the API layer.
///
/// Produced by [`PageLinkRepository::list_backlinks_to`]: each row is a
/// page that links *to* the queried target, joined against the `pages`
/// table so the response can render a clickable list without follow-up
/// lookups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklinkRow {
    /// Source page ID — the page that contains the wikilink.
    pub source_page_id: PageId,
    /// Namespace ID the source page lives in.
    pub source_namespace_id: NamespaceId,
    /// Namespace slug the source page lives in (joined for convenience).
    pub source_namespace_slug: String,
    /// URL slug of the source page.
    pub source_page_slug: String,
    /// Human-readable title of the source page.
    pub source_page_title: String,
}

/// Persistence operations for the outbound-wikilink graph (#30).
///
/// The single source of truth is the `page_links` table, populated by the
/// API layer on page create / update. The query side is one method:
/// [`list_backlinks_to`](Self::list_backlinks_to) — "who links to this
/// `(namespace, slug)`?".
///
/// Mutation lives behind two methods:
///
/// * [`replace_for_source`](Self::replace_for_source) — atomically swap the
///   set of outbound links for a given source page. Called from the page
///   create / update path so the graph always reflects the current
///   revision's `[[Target]]` set.
/// * [`delete_for_source`](Self::delete_for_source) — drop every row for a
///   source page (the `ON DELETE CASCADE` on `page_links.source_page_id`
///   already handles physical deletion; this is the explicit handle).
pub trait PageLinkRepository: Send + Sync {
    /// Replace every outbound link for `source_page_id` with `links`.
    ///
    /// Idempotent: the existing rows are deleted before the new ones are
    /// inserted, so callers do not need to track the previous state. The
    /// caller passes the **resolved** `(target_namespace_slug,
    /// target_page_slug)` pairs; the repository does not parse Markdown.
    fn replace_for_source(
        &self,
        source_page_id: PageId,
        links: &[PageLink],
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Drop every outbound link for `source_page_id`. Used when the page is
    /// emptied or deleted out-of-band.
    fn delete_for_source(
        &self,
        source_page_id: PageId,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// List the pages that link to the `(namespace_slug, page_slug)` pair.
    ///
    /// Order is `(source_page_id ASC)` — stable, deterministic, and aligned
    /// with the UUIDv7 byte prefix so a btree scan stays sequential. The
    /// cursor encodes the last `source_page_id` returned so the next call
    /// resumes strictly after it.
    ///
    /// # Errors
    ///
    /// [`StorageError::InvalidInput`] if `cursor` is malformed for this
    /// backend.
    fn list_backlinks_to(
        &self,
        target_namespace_slug: &str,
        target_page_slug: &str,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> impl Future<Output = Result<PageSlice<BacklinkRow>, StorageError>> + Send;
}

/// Persistence operations for the [`Media`] aggregate (#32).
///
/// Stores **metadata only** — the blob payload lives in a separate place
/// (the `media_blobs` table when the DB backend is selected, or an
/// `object_store`-backed bucket otherwise). The two are kept in separate
/// traits so the S3 path doesn't have to implement a trait method it never
/// uses, and so the DB-backed path can plug in a single connection without
/// dragging an object-store handle through.
pub trait MediaRepository: Send + Sync {
    /// Insert a media metadata row.
    ///
    /// # Errors
    ///
    /// * [`StorageError::Conflict`] if a row with the same `content_hash`
    ///   already exists. Callers normally call
    ///   [`get_by_content_hash`](Self::get_by_content_hash) first to
    ///   deduplicate before reaching this point, but the unique-constraint
    ///   path stays as a defence in depth.
    /// * [`StorageError::Database`] for FK / driver failures (e.g. the
    ///   `uploaded_by` user doesn't exist).
    fn create(&self, media: &Media) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Fetch a media row by primary key.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if no row matches.
    fn get_by_id(&self, id: MediaId) -> impl Future<Output = Result<Media, StorageError>> + Send;

    /// Resolve a media row by its content hash, for deduplication.
    ///
    /// Returns `Ok(None)` rather than [`StorageError::NotFound`] when no
    /// row matches — the caller's normal flow is "look up, then insert",
    /// and a missing row is the expected case.
    ///
    /// # Errors
    ///
    /// Lower-level driver failures propagate as [`StorageError::Database`].
    fn get_by_content_hash(
        &self,
        content_hash: &[u8; 32],
    ) -> impl Future<Output = Result<Option<Media>, StorageError>> + Send;

    /// Delete a media row.
    ///
    /// Does not touch the blob backend — the API layer is responsible for
    /// also calling the configured [`MediaBlobRepository::delete`] (or its
    /// `object_store` equivalent) before / after removing the row. For the
    /// DB backend the `ON DELETE CASCADE` on `media_blobs` takes care of
    /// the paired row automatically.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if the row didn't exist.
    fn delete(&self, id: MediaId) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Stream every media row, ordered by id ascending (`(created_at, id)`
    /// in practice — UUIDv7 sorts lexicographically by creation time).
    ///
    /// Used by the `regen-thumbnails` CLI to walk the table without
    /// holding the whole result set in memory. `cursor` is the last id
    /// returned by the previous call; pass `None` to start fresh.
    /// Backends clamp `limit` via [`clamp_limit`].
    ///
    /// # Errors
    ///
    /// Lower-level driver failures propagate as [`StorageError::Database`].
    fn list_all(
        &self,
        cursor: Option<MediaId>,
        limit: u32,
    ) -> impl Future<Output = Result<PageSlice<Media>, StorageError>> + Send;
}

/// A thumbnail variant row from `media_variants` (#33).
///
/// The `data` column is populated only for the in-DB blob backend; for the
/// S3 backend the variant payload lives in the bucket and `data` is `None`.
/// Variants are content-addressed under the parent `media_id` — the row is
/// dropped automatically via `ON DELETE CASCADE` when the parent media row
/// is removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaVariant {
    /// Parent media row.
    pub media_id: MediaId,
    /// Variant label — `"small"`, `"medium"`, or `"large"`.
    pub variant: String,
    /// IANA media type of the variant bytes (e.g. `image/webp`).
    pub content_type: String,
    /// Stored variant length in bytes.
    pub byte_size: u64,
    /// Rendered width in pixels.
    pub width: u32,
    /// Rendered height in pixels.
    pub height: u32,
    /// Variant payload. `None` when the S3 backend owns the bytes.
    pub data: Option<bytes::Bytes>,
    /// When the variant was generated.
    pub created_at: OffsetDateTime,
}

/// Persistence operations for the thumbnail variants table (#33).
///
/// The trait covers metadata-and-bytes together because the in-DB backend
/// stores both in the same row, and the S3 backend keeps the metadata row
/// even when the payload lives in the bucket. Callers consult `data` to
/// decide whether to serve the row directly or fetch from the object
/// store.
pub trait MediaVariantRepository: Send + Sync {
    /// Insert or replace a variant row. Idempotent on `(media_id, variant)`
    /// so the regen-thumbnails CLI can re-run safely.
    ///
    /// # Errors
    ///
    /// * [`StorageError::Database`] if `media_id` doesn't reference an
    ///   existing media row, or on any driver failure.
    fn put(&self, variant: &MediaVariant) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Fetch a specific variant for `media_id`.
    ///
    /// Returns `Ok(None)` if the variant doesn't exist (the caller's
    /// fallback is to serve the original).
    ///
    /// # Errors
    ///
    /// Lower-level driver failures propagate as [`StorageError::Database`].
    fn get(
        &self,
        media_id: MediaId,
        variant: &str,
    ) -> impl Future<Output = Result<Option<MediaVariant>, StorageError>> + Send;

    /// Drop every variant for `media_id`. Called by the regen-thumbnails
    /// path before re-inserting fresh rows so we don't keep stale data
    /// around with the wrong dimensions.
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures.
    fn delete_for_media(
        &self,
        media_id: MediaId,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;
}

/// Persistence operations for the in-database blob backend.
///
/// Only implemented by adapters that ship the `media_blobs` table. The S3
/// backend uses `object_store` directly and never reaches for this trait.
///
/// Payload is moved as [`bytes::Bytes`] to avoid copying on the way through
/// — backends `clone()` is a cheap refcount bump.
pub trait MediaBlobRepository: Send + Sync {
    /// Store the blob bytes for `media_id`.
    ///
    /// # Errors
    ///
    /// * [`StorageError::Database`] if `media_id` doesn't reference an
    ///   existing media row (FK violation) or on any driver failure.
    fn put(
        &self,
        media_id: MediaId,
        data: bytes::Bytes,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Fetch the blob bytes for `media_id`.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if no row matches.
    fn get(
        &self,
        media_id: MediaId,
    ) -> impl Future<Output = Result<bytes::Bytes, StorageError>> + Send;

    /// Delete the blob bytes for `media_id`. Idempotent: deleting a row
    /// that doesn't exist is a no-op (the API layer keeps the media-row
    /// and blob-row deletes in lockstep, but a stray blob row from a
    /// half-failed upload should still be reapable).
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures.
    fn delete(&self, media_id: MediaId) -> impl Future<Output = Result<(), StorageError>> + Send;
}

/// A `(page_id, namespace_slug, page_slug, title)` tuple surfaced by the
/// "list pages in category" / "list pages with tag" queries.
///
/// Same shape as [`BacklinkRow`] but the source is the category / tag join,
/// not the wikilink graph. Carved out so the API layer can render a
/// member-page list without a follow-up lookup per row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMemberRow {
    /// Stable identifier of the member page.
    pub page_id: PageId,
    /// Namespace ID the page lives in.
    pub namespace_id: NamespaceId,
    /// Namespace slug the page lives in (joined for convenience).
    pub namespace_slug: String,
    /// URL slug of the page.
    pub page_slug: String,
    /// Human-readable title of the page.
    pub page_title: String,
}

/// Persistence operations for the [`Category`] aggregate (#29).
///
/// Categories form a DAG: each category has at most one explicit parent,
/// and a page can belong to many categories. Cycle prevention is the
/// repository's responsibility — `create` and any future `set_parent`
/// helpers walk the would-be ancestor chain and reject mutations that
/// would re-introduce the current node as its own ancestor.
pub trait CategoryRepository: Send + Sync {
    /// Insert a category row.
    ///
    /// # Errors
    ///
    /// * [`StorageError::Conflict`] if the slug is already taken or the
    ///   provided `parent_id` would create a cycle.
    /// * [`StorageError::NotFound`] if the supplied `parent_id` doesn't
    ///   resolve to an existing category.
    fn create(&self, category: &Category) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Fetch a category by primary key.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if no row matches.
    fn get_by_id(
        &self,
        id: CategoryId,
    ) -> impl Future<Output = Result<Category, StorageError>> + Send;

    /// Resolve a category by its URL slug.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if no row matches.
    fn get_by_slug(
        &self,
        slug: &str,
    ) -> impl Future<Output = Result<Category, StorageError>> + Send;

    /// List every category, ordered by slug ascending.
    ///
    /// The category count is expected to stay small (operator-curated set)
    /// so pagination is not necessary today.
    fn list_all(&self) -> impl Future<Output = Result<Vec<Category>, StorageError>> + Send;

    /// List direct children of `parent`. `None` returns top-level
    /// categories (rows whose `parent_id` is `NULL`).
    fn list_children(
        &self,
        parent: Option<CategoryId>,
    ) -> impl Future<Output = Result<Vec<Category>, StorageError>> + Send;

    /// Walk the ancestor chain of `id` upwards, starting with the immediate
    /// parent and ending at the top-level ancestor. Returns an empty vector
    /// when the category has no parent. Used for cycle prevention and for
    /// rendering the breadcrumb of the `/category/<slug>` page.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if `id` itself does not exist.
    fn list_ancestors(
        &self,
        id: CategoryId,
    ) -> impl Future<Output = Result<Vec<Category>, StorageError>> + Send;

    /// Add `(page_id, category_id)` to the membership table. Idempotent
    /// (re-assigning the same pair is a no-op).
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] if either foreign key doesn't resolve.
    fn assign_to_page(
        &self,
        page_id: PageId,
        category_id: CategoryId,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Remove `(page_id, category_id)` from the membership table.
    /// Idempotent.
    fn unassign_from_page(
        &self,
        page_id: PageId,
        category_id: CategoryId,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// Replace the entire category set for `page_id` with `categories`.
    /// Atomic: the existing rows are dropped and the new set inserted
    /// inside a single transaction so a reader never sees a half-applied
    /// edit.
    ///
    /// # Errors
    ///
    /// [`StorageError::Database`] if any category foreign key doesn't
    /// resolve. The transaction rolls back on failure so partial
    /// assignment is impossible.
    fn replace_for_page(
        &self,
        page_id: PageId,
        categories: &[CategoryId],
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// List the categories `page_id` is assigned to, ordered by slug.
    fn list_for_page(
        &self,
        page_id: PageId,
    ) -> impl Future<Output = Result<Vec<Category>, StorageError>> + Send;

    /// List the pages assigned to `category_id`, paginated.
    ///
    /// Order is `(page_id ASC)` — stable, deterministic, and aligned with
    /// the UUIDv7 byte prefix so the index walk stays sequential.
    fn list_pages_in(
        &self,
        category_id: CategoryId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> impl Future<Output = Result<PageSlice<PageMemberRow>, StorageError>> + Send;
}

/// Persistence operations for page tags (#29).
///
/// Tags are flat lowercased strings keyed by [`Tag`] at the validation
/// boundary. Storage stores them as raw `TEXT` (lowercased) — `Tag` is the
/// caller-side guarantee that the wire form has been validated.
pub trait TagRepository: Send + Sync {
    /// Replace the entire tag set for `page_id` with `tags`. Atomic: the
    /// existing rows are dropped and the new set inserted inside a single
    /// transaction so a reader never sees a half-applied edit.
    ///
    /// Duplicates inside `tags` are silently coalesced (the join table's
    /// primary key already deduplicates).
    fn assign(
        &self,
        page_id: PageId,
        tags: &[Tag],
    ) -> impl Future<Output = Result<(), StorageError>> + Send;

    /// List the tags assigned to `page_id`, ordered lexicographically.
    fn list_for_page(
        &self,
        page_id: PageId,
    ) -> impl Future<Output = Result<Vec<Tag>, StorageError>> + Send;

    /// List the pages carrying `tag`, paginated.
    ///
    /// Order is `(page_id ASC)`. The cursor encodes the last `page_id`
    /// returned so the next call resumes strictly after it.
    fn list_pages_with_tag(
        &self,
        tag: &Tag,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> impl Future<Output = Result<PageSlice<PageMemberRow>, StorageError>> + Send;

    /// Enumerate the distinct tag values whose lowercase form starts with
    /// `prefix`, sorted lexicographically. `limit` clamps the result set
    /// for the autocomplete endpoint.
    ///
    /// `prefix` is *not* required to be a fully-validated [`Tag`] — a UI
    /// autocomplete sends whatever the user has typed so far. Callers
    /// normalise to lowercase before binding.
    fn list_all_tags(
        &self,
        prefix: &str,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<Tag>, StorageError>> + Send;
}

/// One persisted row from the IP blocklist (#42).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpBlocklistEntry {
    /// UUIDv7 primary key.
    pub id: uuid::Uuid,
    /// CIDR string in its canonical human form (`203.0.113.0/24`,
    /// `2001:db8::/32`). The repository stores whatever the caller hands it;
    /// validation happens above the storage layer.
    pub cidr: String,
    /// Free-form reason. Stored as the empty string when the caller didn't
    /// supply one.
    pub reason: String,
    /// User who created the row.
    pub created_by: UserId,
    /// When the row was created.
    pub created_at: OffsetDateTime,
}

/// One persisted row from the URL blocklist (#42).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlBlocklistEntry {
    /// UUIDv7 primary key.
    pub id: uuid::Uuid,
    /// Rust `regex` crate pattern. The caller is expected to have
    /// successfully compiled this with `regex::Regex::new` before persisting
    /// — the storage layer treats it as opaque text.
    pub pattern: String,
    /// Free-form reason.
    pub reason: String,
    /// User who created the row.
    pub created_by: UserId,
    /// When the row was created.
    pub created_at: OffsetDateTime,
}

/// Input for inserting an IP blocklist row.
#[derive(Debug, Clone)]
pub struct NewIpBlocklistEntry {
    /// CIDR in its canonical form.
    pub cidr: String,
    /// Free-form reason (`""` is fine).
    pub reason: String,
    /// User creating the entry.
    pub created_by: UserId,
}

/// Input for inserting a URL blocklist row.
#[derive(Debug, Clone)]
pub struct NewUrlBlocklistEntry {
    /// Validated `regex` pattern.
    pub pattern: String,
    /// Free-form reason.
    pub reason: String,
    /// User creating the entry.
    pub created_by: UserId,
}

/// Persistence operations for the IP blocklist (#42).
///
/// The blocklist is loaded into memory on boot (and on every mutation) by
/// the API layer; queries against this repository do not happen on the
/// request hot path. Listing is unpaginated because operator-curated
/// blocklists stay small in v1.
pub trait IpBlocklistRepository: Send + Sync {
    /// Insert a row and return the stored entry.
    ///
    /// # Errors
    ///
    /// * [`StorageError::Conflict`] if `cidr` already exists.
    /// * [`StorageError::Database`] on driver failure.
    fn create(
        &self,
        entry: NewIpBlocklistEntry,
    ) -> impl Future<Output = Result<IpBlocklistEntry, StorageError>> + Send;

    /// List every row, ordered newest first.
    fn list_all(&self)
    -> impl Future<Output = Result<Vec<IpBlocklistEntry>, StorageError>> + Send;

    /// Fetch a single row by primary key.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if the row didn't exist. Used by the admin
    /// delete handler to capture the CIDR for the audit log before the row is
    /// removed.
    fn get_by_id(
        &self,
        id: uuid::Uuid,
    ) -> impl Future<Output = Result<IpBlocklistEntry, StorageError>> + Send;

    /// Delete a row by primary key.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if the row didn't exist.
    fn delete(&self, id: uuid::Uuid) -> impl Future<Output = Result<(), StorageError>> + Send;
}

/// Persistence operations for the URL blocklist (#42).
pub trait UrlBlocklistRepository: Send + Sync {
    /// Insert a row and return the stored entry.
    ///
    /// # Errors
    ///
    /// * [`StorageError::Conflict`] if `pattern` already exists.
    /// * [`StorageError::Database`] on driver failure.
    fn create(
        &self,
        entry: NewUrlBlocklistEntry,
    ) -> impl Future<Output = Result<UrlBlocklistEntry, StorageError>> + Send;

    /// List every row, ordered newest first.
    fn list_all(
        &self,
    ) -> impl Future<Output = Result<Vec<UrlBlocklistEntry>, StorageError>> + Send;

    /// Fetch a single row by primary key.
    ///
    /// # Errors
    ///
    /// [`StorageError::NotFound`] if the row didn't exist. Used by the admin
    /// delete handler to capture the pattern for the audit log before the row
    /// is removed.
    fn get_by_id(
        &self,
        id: uuid::Uuid,
    ) -> impl Future<Output = Result<UrlBlocklistEntry, StorageError>> + Send;

    /// Delete a row by primary key.
    fn delete(&self, id: uuid::Uuid) -> impl Future<Output = Result<(), StorageError>> + Send;
}

/// A flattened row in the per-user watchlist.
///
/// Each row stands on its own — page slug, namespace slug, and title are
/// joined in so the API can render the `/watchlist` listing and the
/// `watchlist.atom` feed without follow-up lookups.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedPage {
    /// Watched page.
    pub page_id: PageId,
    /// Namespace the page lives in.
    pub namespace_id: NamespaceId,
    /// Slug of the namespace, joined for convenience.
    pub namespace_slug: String,
    /// URL slug of the page.
    pub page_slug: String,
    /// Human-readable title of the page.
    pub page_title: String,
    /// `pages.protection_level` snapshot.
    pub protection_level: ProtectionLevel,
    /// When the user added the page to their watchlist.
    pub watched_at: OffsetDateTime,
    /// When the page itself was last updated. Drives the Atom `<updated>`
    /// for the feed entry.
    pub updated_at: OffsetDateTime,
}

/// Persistence operations for the per-user watchlist (#46).
///
/// Each `(user_id, page_id)` pair represents an opt-in subscription to the
/// page's revision feed. Watchlist reads are scoped to the caller's own
/// rows; the trait is intentionally narrow because no admin tool yet
/// needs cross-user visibility.
pub trait WatchRepository: Send + Sync {
    /// Add a `(user_id, page_id)` row. Idempotent — re-watching a page the
    /// user already watches is a no-op and the existing `created_at` is
    /// preserved.
    ///
    /// Returns `true` if a new row was inserted and `false` if the user was
    /// already watching the page. The caller (e.g. the audit log emitter)
    /// uses this to avoid recording spurious "added" events for retries.
    ///
    /// # Errors
    ///
    /// * [`StorageError::Database`] if either foreign key doesn't resolve
    ///   (the user or the page doesn't exist).
    fn watch(
        &self,
        user_id: UserId,
        page_id: PageId,
    ) -> impl Future<Output = Result<bool, StorageError>> + Send;

    /// Remove the `(user_id, page_id)` row. Idempotent — removing a row
    /// that isn't there is a no-op.
    ///
    /// Returns `true` if a row was actually removed and `false` if the user
    /// wasn't watching the page in the first place. Same caller contract as
    /// [`watch`](Self::watch).
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures as [`StorageError::Database`].
    fn unwatch(
        &self,
        user_id: UserId,
        page_id: PageId,
    ) -> impl Future<Output = Result<bool, StorageError>> + Send;

    /// Report whether `user_id` currently watches `page_id`.
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures as [`StorageError::Database`].
    fn is_watched(
        &self,
        user_id: UserId,
        page_id: PageId,
    ) -> impl Future<Output = Result<bool, StorageError>> + Send;

    /// List the pages `user_id` watches, joined against the page + namespace
    /// rows for direct rendering. Order is `(watched_at DESC, page_id DESC)`
    /// so the newest subscription appears first.
    ///
    /// # Errors
    ///
    /// Propagates lower-level driver failures as [`StorageError::Database`].
    fn list_for_user(
        &self,
        user_id: UserId,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<WatchedPage>, StorageError>> + Send;
}
