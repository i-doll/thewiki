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
    AuditLogId, Namespace, NamespaceId, NamespaceSlug, Page, PageId, Revision, RevisionId, Role,
    RoleId, RoleName, Session, SessionId, User, UserId, Username,
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
