//! Application state passed to Axum handlers.
//!
//! [`AppState`] carries a single cloneable storage handle plus runtime tuning
//! the route handlers need (e.g. the default page-list size). It is generic
//! over the storage facade so the API crate stays decoupled from the
//! concrete SQLite/Postgres/libsql implementation.
//!
//! ## Why a trait instead of `Arc<dyn …>`?
//!
//! The repository traits in [`thewiki_storage::repo`] use native
//! `async fn` in trait, which is not `dyn`-compatible on stable today (see the
//! design note in `crates/storage/src/repo.rs`). The intentional path is
//! monomorphisation via generics: [`AppStorage`] abstracts over the *storage
//! handle* (a cheap-to-clone facade like
//! [`thewiki_storage::sqlite::SqliteStorage`]), and each handler reaches for
//! the per-aggregate repository via the accessor methods.

use std::sync::Arc;

use axum::extract::FromRef;
use thewiki_storage::StorageError;
use thewiki_storage::repo::{
    AuditLogRepository, NamespaceRepository, NewAuditLogEntry, PageAuditMutation, PageRepository,
    RecentChangesRepository, RevisionRepository, UserRepository,
};

use crate::auth::AuthState;
use crate::config::AuthConfig;

/// A cloneable storage facade that hands out per-aggregate repositories.
///
/// Implemented by the concrete storage facades (e.g.
/// [`thewiki_storage::sqlite::SqliteStorage`]) so the API layer can stay
/// generic. The lifetime-bound associated types mirror the pattern the
/// SQLite repositories use today — repositories borrow the pool for the
/// duration of the call rather than each owning their own `Arc<Pool>`.
pub trait AppStorage: Clone + Send + Sync + 'static {
    /// Page repository borrowed from this handle.
    type Pages<'a>: PageRepository + 'a
    where
        Self: 'a;
    /// Revision repository borrowed from this handle.
    type Revisions<'a>: RevisionRepository + 'a
    where
        Self: 'a;
    /// Namespace repository borrowed from this handle.
    type Namespaces<'a>: NamespaceRepository + 'a
    where
        Self: 'a;
    /// Recent-changes repository borrowed from this handle.
    type RecentChanges<'a>: RecentChangesRepository + 'a
    where
        Self: 'a;
    /// Audit-log repository borrowed from this handle.
    type AuditLog<'a>: AuditLogRepository + 'a
    where
        Self: 'a;
    /// User repository borrowed from this handle.
    type Users<'a>: UserRepository + 'a
    where
        Self: 'a;

    /// Borrow a [`PageRepository`].
    fn pages(&self) -> Self::Pages<'_>;
    /// Borrow a [`RevisionRepository`].
    fn revisions(&self) -> Self::Revisions<'_>;
    /// Borrow a [`NamespaceRepository`].
    fn namespaces(&self) -> Self::Namespaces<'_>;
    /// Borrow a [`RecentChangesRepository`].
    fn recent_changes(&self) -> Self::RecentChanges<'_>;
    /// Borrow an [`AuditLogRepository`].
    fn audit_log(&self) -> Self::AuditLog<'_>;
    /// Borrow a [`UserRepository`].
    fn users(&self) -> Self::Users<'_>;

    /// Commit a page mutation and its required audit row atomically.
    fn commit_page_audit(
        &self,
        mutation: PageAuditMutation,
        audit: NewAuditLogEntry,
    ) -> impl Future<Output = Result<(), StorageError>> + Send;
}

impl AppStorage for thewiki_storage::sqlite::SqliteStorage {
    type Pages<'a> = thewiki_storage::sqlite::SqlitePageRepository<'a>;
    type Revisions<'a> = thewiki_storage::sqlite::SqliteRevisionRepository<'a>;
    type Namespaces<'a> = thewiki_storage::sqlite::SqliteNamespaceRepository<'a>;
    type RecentChanges<'a> = thewiki_storage::sqlite::SqliteRecentChangesRepository<'a>;
    type AuditLog<'a> = thewiki_storage::sqlite::SqliteAuditLogRepository<'a>;
    type Users<'a> = thewiki_storage::sqlite::SqliteUserRepository<'a>;

    fn pages(&self) -> Self::Pages<'_> {
        Self::pages(self)
    }
    fn revisions(&self) -> Self::Revisions<'_> {
        Self::revisions(self)
    }
    fn namespaces(&self) -> Self::Namespaces<'_> {
        Self::namespaces(self)
    }
    fn recent_changes(&self) -> Self::RecentChanges<'_> {
        Self::recent_changes(self)
    }
    fn audit_log(&self) -> Self::AuditLog<'_> {
        Self::audit_log(self)
    }
    fn users(&self) -> Self::Users<'_> {
        Self::users(self)
    }

    fn commit_page_audit(
        &self,
        mutation: PageAuditMutation,
        audit: NewAuditLogEntry,
    ) -> impl Future<Output = Result<(), StorageError>> + Send {
        Self::commit_page_audit(self, mutation, audit)
    }
}

/// Routing-time tuning knobs.
///
/// Carved out of [`crate::config::Config`] so the route layer doesn't reach
/// back into the binary's full config tree. Cheap to clone.
#[derive(Debug, Clone, Copy)]
pub struct RouteConfig {
    /// Page size used when a list endpoint receives no `limit` query param.
    /// Hard upper bound is enforced by the storage layer
    /// ([`thewiki_storage::repo::MAX_PAGE_SIZE`]).
    pub default_page_size: u32,
}

impl Default for RouteConfig {
    fn default() -> Self {
        Self {
            default_page_size: thewiki_storage::repo::DEFAULT_PAGE_SIZE,
        }
    }
}

/// Application state, cloned cheaply into every request.
///
/// `S` is the storage facade (typically
/// [`thewiki_storage::sqlite::SqliteStorage`]); see [`AppStorage`].
///
/// The `auth_config` snapshot is the wired runtime view of
/// [`crate::config::AuthConfig`] — handlers consult it to decide whether to
/// require a session, whether to gate edits into the (M2) approval queue, and
/// what registration policy to advertise via `GET /api/v1/auth/policy`.
///
/// `auth_state` is optional because some integration tests boot just the
/// pages router without standing up the auth stack. When `None`, the
/// configurable-auth extractors fall back to "no session was supplied" — i.e.
/// every caller is treated as anonymous. Production (`build_full`) always
/// supplies one.
pub struct AppState<S: AppStorage> {
    /// Shared storage handle.
    pub storage: Arc<S>,
    /// Per-route configuration knobs.
    pub route_config: RouteConfig,
    /// Snapshot of `Config::auth` — the configurable-auth wiring point (#14).
    pub auth_config: AuthConfig,
    /// Auth state shared with the auth router (cookies, hasher, session TTL).
    /// `None` in test fixtures that don't exercise the auth stack.
    pub auth_state: Option<AuthState>,
}

impl<S: AppStorage> AppState<S> {
    /// Build a new [`AppState`] from a storage handle and the configured auth
    /// snapshot. The default route config is applied; the auth state is
    /// initialised to `None` (suitable for tests that don't stand up the auth
    /// stack — production wiring uses [`Self::with_auth_state`]).
    #[must_use]
    pub fn new(storage: S, auth_config: AuthConfig) -> Self {
        Self {
            storage: Arc::new(storage),
            route_config: RouteConfig::default(),
            auth_config,
            auth_state: None,
        }
    }

    /// Convenience for tests: build a state with the built-in default
    /// [`AuthConfig`] (closed registration, no anonymous edits, no approval
    /// queue). Production wiring uses [`Self::new`] with the operator-supplied
    /// config.
    #[must_use]
    pub fn new_with_defaults(storage: S) -> Self {
        Self::new(storage, crate::config::Config::defaults().auth)
    }

    /// Attach an [`AuthState`] so the configurable-auth extractors can resolve
    /// session cookies against the auth stack.
    #[must_use]
    pub fn with_auth_state(mut self, auth: AuthState) -> Self {
        self.auth_state = Some(auth);
        self
    }

    /// Override the default page size used by list endpoints.
    #[must_use]
    pub fn with_default_page_size(mut self, n: u32) -> Self {
        self.route_config.default_page_size = n;
        self
    }
}

impl<S: AppStorage> Clone for AppState<S> {
    fn clone(&self) -> Self {
        Self {
            storage: Arc::clone(&self.storage),
            route_config: self.route_config,
            auth_config: self.auth_config.clone(),
            auth_state: self.auth_state.clone(),
        }
    }
}

/// Expose the optional [`AuthState`] for axum's `State<AuthState>` extractor.
///
/// Panics if no auth state has been wired — that's a configuration bug at
/// router-construction time, not a per-request failure. Pages handlers go
/// through the configurable-auth extractors (see
/// [`crate::extractors::EditorExtractor`]) which handle the missing-auth case
/// gracefully and treat it as "anonymous caller".
impl<S: AppStorage> FromRef<AppState<S>> for AuthState {
    fn from_ref(input: &AppState<S>) -> Self {
        #[allow(
            clippy::expect_used,
            reason = "router wiring guarantees auth_state is present whenever an auth-state \
                      extractor is reachable; missing it here is a misconfiguration the dev \
                      should see loudly"
        )]
        input
            .auth_state
            .clone()
            .expect("AppState was constructed without an AuthState but a handler requires it")
    }
}
