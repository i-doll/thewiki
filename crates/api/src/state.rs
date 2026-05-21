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

use thewiki_storage::repo::{NamespaceRepository, PageRepository, RevisionRepository};

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

    /// Borrow a [`PageRepository`].
    fn pages(&self) -> Self::Pages<'_>;
    /// Borrow a [`RevisionRepository`].
    fn revisions(&self) -> Self::Revisions<'_>;
    /// Borrow a [`NamespaceRepository`].
    fn namespaces(&self) -> Self::Namespaces<'_>;
}

impl AppStorage for thewiki_storage::sqlite::SqliteStorage {
    type Pages<'a> = thewiki_storage::sqlite::SqlitePageRepository<'a>;
    type Revisions<'a> = thewiki_storage::sqlite::SqliteRevisionRepository<'a>;
    type Namespaces<'a> = thewiki_storage::sqlite::SqliteNamespaceRepository<'a>;

    fn pages(&self) -> Self::Pages<'_> {
        Self::pages(self)
    }
    fn revisions(&self) -> Self::Revisions<'_> {
        Self::revisions(self)
    }
    fn namespaces(&self) -> Self::Namespaces<'_> {
        Self::namespaces(self)
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
pub struct AppState<S: AppStorage> {
    /// Shared storage handle.
    pub storage: Arc<S>,
    /// Per-route configuration knobs.
    pub route_config: RouteConfig,
}

impl<S: AppStorage> AppState<S> {
    /// Build a new [`AppState`] from a storage handle and default route config.
    #[must_use]
    pub fn new(storage: S) -> Self {
        Self {
            storage: Arc::new(storage),
            route_config: RouteConfig::default(),
        }
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
        }
    }
}
