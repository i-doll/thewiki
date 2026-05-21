//! Persistence traits — one per domain aggregate.
//!
//! The shape of every trait is the same: async methods returning
//! [`Result<T, StorageError>`](crate::StorageError). Backends implement these;
//! the API crate consumes them through `Arc<dyn …>` and stays backend-agnostic
//! at the call site.
//!
//! Pagination is **cursor-based** — list endpoints accept a [`Cursor`] and a
//! `limit`, and return a slice plus an opaque next-cursor token. Offset
//! pagination is intentionally avoided so large tables don't degrade and so
//! the wire form stays stable across sorted-by changes.

use thewiki_core::{
    Namespace, NamespaceId, NamespaceSlug, Page, PageId, Revision, RevisionId, Role, RoleId,
    RoleName, User, UserId, Username,
};

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

/// Clamp a caller-supplied `limit` to the configured page-size bounds.
#[must_use]
pub fn clamp_limit(limit: u32) -> u32 {
    let l = if limit == 0 { DEFAULT_PAGE_SIZE } else { limit };
    l.min(MAX_PAGE_SIZE)
}
