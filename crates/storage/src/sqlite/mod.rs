//! SQLite-backed implementations of the [`Repository`](crate::repo) traits.
//!
//! The entry point is [`SqliteStorage`], which owns a [`sqlx::SqlitePool`]
//! and exposes one repository handle per aggregate via accessor methods.
//!
//! ## Pool & options
//!
//! Connection pooling is configured through [`SqliteOptions`]:
//!
//! ```no_run
//! # async fn doc() -> Result<(), thewiki_storage::StorageError> {
//! use thewiki_storage::sqlite::{SqliteOptions, SqliteStorage};
//! use std::time::Duration;
//!
//! let storage = SqliteStorage::new(
//!     "sqlite::memory:",
//!     SqliteOptions {
//!         max_connections: 4,
//!         acquire_timeout: Duration::from_secs(5),
//!         foreign_keys: true,
//!     },
//! )
//! .await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Foreign keys
//!
//! sqlx's SQLite driver does **not** enable foreign-key enforcement by
//! default. [`SqliteStorage::new`] turns it on explicitly via
//! [`SqliteConnectOptions::foreign_keys`](sqlx::sqlite::SqliteConnectOptions::foreign_keys);
//! the schema relies on it.
//!
//! ## Type mappings
//!
//! | Domain                 | Column           | Encoding                                 |
//! |------------------------|------------------|------------------------------------------|
//! | `*Id` (UUIDv7)         | `BLOB(16)`       | `Uuid::as_bytes()`                       |
//! | `OffsetDateTime`       | `TEXT`           | RFC 3339                                 |
//! | `Permissions` (u32)    | `INTEGER`        | `bits() as i64`                          |
//! | `ContentFormat`        | `TEXT`           | `as_str()`                               |
//! | `ProtectionLevel`      | `TEXT`           | `as_str()`                               |
//!
//! We deliberately use runtime-checked `sqlx::query`/`sqlx::query_as` (rather
//! than the compile-time-checked macros) so the build doesn't require a live
//! `DATABASE_URL` or a checked-in `sqlx-data.json`. Migration to offline mode
//! is tracked as a follow-up.

use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

use crate::error::StorageError;

mod audit_log;
mod category;
mod codec;
mod ip_blocklist;
mod media;
mod namespace;
mod notification;
mod page;
mod page_audit;
mod page_link;
mod pending_revision;
mod recent_changes;
mod revision;
mod role;
mod session;
mod tag;
mod url_blocklist;
mod user;
mod watch;

pub use audit_log::SqliteAuditLogRepository;
pub use category::SqliteCategoryRepository;
pub use ip_blocklist::SqliteIpBlocklistRepository;
pub use media::{SqliteMediaBlobRepository, SqliteMediaRepository, SqliteMediaVariantRepository};
pub use namespace::SqliteNamespaceRepository;
pub use notification::SqliteNotificationRepository;
pub use page::SqlitePageRepository;
pub use page_link::SqlitePageLinkRepository;
pub use pending_revision::SqlitePendingRevisionRepository;
pub use recent_changes::SqliteRecentChangesRepository;
pub use revision::SqliteRevisionRepository;
pub use role::SqliteRoleRepository;
pub use session::SqliteSessionRepository;
pub use tag::SqliteTagRepository;
pub use url_blocklist::SqliteUrlBlocklistRepository;
pub use user::SqliteUserRepository;
pub use watch::SqliteWatchRepository;

/// Migration set baked into the binary at compile time. See `/migrations/`.
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

/// Tuning knobs for the [`SqlitePool`] backing [`SqliteStorage`].
///
/// All fields are configurable from app config; see [`Self::default`] for the
/// shipped defaults.
#[derive(Debug, Clone)]
pub struct SqliteOptions {
    /// Maximum number of connections the pool will hold open.
    pub max_connections: u32,
    /// How long a caller waits for a free connection before `acquire` errors.
    pub acquire_timeout: Duration,
    /// Whether to enable SQLite foreign-key enforcement (`PRAGMA foreign_keys = ON`).
    ///
    /// `true` matches the schema's assumptions and the [`SqliteStorage`]
    /// default. Disable only if you have a very specific reason — the schema
    /// relies on FK cascades for `revisions` deletion.
    pub foreign_keys: bool,
}

impl Default for SqliteOptions {
    fn default() -> Self {
        Self {
            max_connections: 5,
            acquire_timeout: Duration::from_secs(30),
            foreign_keys: true,
        }
    }
}

/// SQLite-backed storage facade.
///
/// Holds the pool and dispenses per-aggregate repository handles. Clone is
/// cheap — the inner `SqlitePool` is already an `Arc`.
#[derive(Debug, Clone)]
pub struct SqliteStorage {
    pool: SqlitePool,
}

impl SqliteStorage {
    /// Open a pool against `url`, apply migrations, and return a handle.
    ///
    /// `url` is parsed as a [`SqliteConnectOptions`] connection string, so
    /// `sqlite::memory:`, `sqlite://path/to/file.db`, and bare filesystem
    /// paths are all accepted.
    ///
    /// # Errors
    ///
    /// * [`StorageError::InvalidInput`] if `url` doesn't parse.
    /// * [`StorageError::Database`] on pool / driver failures.
    /// * [`StorageError::Migration`] if the migration set fails to apply.
    pub async fn new(url: &str, opts: SqliteOptions) -> Result<Self, StorageError> {
        let connect_opts: SqliteConnectOptions = url
            .parse()
            .map_err(|err: sqlx::Error| StorageError::invalid_input(err.to_string()))?;
        // Foreign keys are off in sqlx by default; the schema relies on FK
        // cascades, so opt in explicitly.
        let connect_opts = connect_opts.foreign_keys(opts.foreign_keys);

        let pool = SqlitePoolOptions::new()
            .max_connections(opts.max_connections)
            .acquire_timeout(opts.acquire_timeout)
            .connect_with(connect_opts)
            .await?;

        MIGRATOR
            .run(&pool)
            .await
            .map_err(|err| StorageError::Migration(err.to_string()))?;

        Ok(Self { pool })
    }

    /// Construct a [`SqliteStorage`] around an already-built pool.
    ///
    /// Useful for tests that want to share a pool across helpers, or for
    /// callers that need finer-grained control over [`SqlitePoolOptions`]
    /// than [`SqliteOptions`] exposes. Migrations are **not** run — the
    /// caller is responsible for getting the schema into place.
    #[must_use]
    pub fn from_pool(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Borrow the underlying pool (for transactional use cases that span
    /// multiple repositories).
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Borrow this handle as a [`PageRepository`](crate::repo::PageRepository).
    #[must_use]
    pub fn pages(&self) -> SqlitePageRepository<'_> {
        SqlitePageRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`RevisionRepository`](crate::repo::RevisionRepository).
    #[must_use]
    pub fn revisions(&self) -> SqliteRevisionRepository<'_> {
        SqliteRevisionRepository::new(&self.pool)
    }

    /// Borrow this handle as a [`UserRepository`](crate::repo::UserRepository).
    #[must_use]
    pub fn users(&self) -> SqliteUserRepository<'_> {
        SqliteUserRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`NamespaceRepository`](crate::repo::NamespaceRepository).
    #[must_use]
    pub fn namespaces(&self) -> SqliteNamespaceRepository<'_> {
        SqliteNamespaceRepository::new(&self.pool)
    }

    /// Borrow this handle as a [`RoleRepository`](crate::repo::RoleRepository).
    #[must_use]
    pub fn roles(&self) -> SqliteRoleRepository<'_> {
        SqliteRoleRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`SessionRepository`](crate::repo::SessionRepository).
    #[must_use]
    pub fn sessions(&self) -> SqliteSessionRepository<'_> {
        SqliteSessionRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`RecentChangesRepository`](crate::repo::RecentChangesRepository).
    #[must_use]
    pub fn recent_changes(&self) -> SqliteRecentChangesRepository<'_> {
        SqliteRecentChangesRepository::new(&self.pool)
    }

    /// Borrow this handle as an [`AuditLogRepository`](crate::repo::AuditLogRepository).
    #[must_use]
    pub fn audit_log(&self) -> SqliteAuditLogRepository<'_> {
        SqliteAuditLogRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`PageLinkRepository`](crate::repo::PageLinkRepository).
    #[must_use]
    pub fn page_links(&self) -> SqlitePageLinkRepository<'_> {
        SqlitePageLinkRepository::new(&self.pool)
    }

    /// Borrow this handle as a [`MediaRepository`](crate::repo::MediaRepository).
    #[must_use]
    pub fn media(&self) -> SqliteMediaRepository<'_> {
        SqliteMediaRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`MediaBlobRepository`](crate::repo::MediaBlobRepository). Only
    /// useful when the configured storage backend is `Db`; the S3 backend
    /// uses `object_store` directly.
    #[must_use]
    pub fn media_blobs(&self) -> SqliteMediaBlobRepository<'_> {
        SqliteMediaBlobRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`MediaVariantRepository`](crate::repo::MediaVariantRepository).
    /// Thumbnails generated by the upload pipeline (#33) land here.
    #[must_use]
    pub fn media_variants(&self) -> SqliteMediaVariantRepository<'_> {
        SqliteMediaVariantRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`CategoryRepository`](crate::repo::CategoryRepository) (#29).
    #[must_use]
    pub fn categories(&self) -> SqliteCategoryRepository<'_> {
        SqliteCategoryRepository::new(&self.pool)
    }

    /// Borrow this handle as a [`TagRepository`](crate::repo::TagRepository) (#29).
    #[must_use]
    pub fn tags(&self) -> SqliteTagRepository<'_> {
        SqliteTagRepository::new(&self.pool)
    }

    /// Borrow this handle as an
    /// [`IpBlocklistRepository`](crate::repo::IpBlocklistRepository) (#42).
    #[must_use]
    pub fn ip_blocklist(&self) -> SqliteIpBlocklistRepository<'_> {
        SqliteIpBlocklistRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`UrlBlocklistRepository`](crate::repo::UrlBlocklistRepository) (#42).
    #[must_use]
    pub fn url_blocklist(&self) -> SqliteUrlBlocklistRepository<'_> {
        SqliteUrlBlocklistRepository::new(&self.pool)
    }

    /// Borrow this handle as a [`WatchRepository`](crate::repo::WatchRepository) (#46).
    #[must_use]
    pub fn watches(&self) -> SqliteWatchRepository<'_> {
        SqliteWatchRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`PendingRevisionRepository`](crate::repo::PendingRevisionRepository) (#40).
    #[must_use]
    pub fn pending_revisions(&self) -> SqlitePendingRevisionRepository<'_> {
        SqlitePendingRevisionRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`NotificationRepository`](crate::repo::NotificationRepository) (#40).
    #[must_use]
    pub fn notifications(&self) -> SqliteNotificationRepository<'_> {
        SqliteNotificationRepository::new(&self.pool)
    }

    /// Commit a page mutation together with its audit-log row.
    ///
    /// The operation runs in a single SQLite transaction so a successful page
    /// mutation cannot be reported without a matching audit entry.
    pub async fn commit_page_audit(
        &self,
        mutation: crate::repo::PageAuditMutation,
        audit: crate::repo::NewAuditLogEntry,
    ) -> Result<(), StorageError> {
        page_audit::commit_page_audit(&self.pool, mutation, audit).await
    }

    /// Apply the embedded migration set to an arbitrary pool.
    ///
    /// Exposed for [`from_pool`](Self::from_pool) callers and integration
    /// tests that build their own pool.
    ///
    /// # Errors
    ///
    /// [`StorageError::Migration`] if any migration fails.
    pub async fn migrate(pool: &SqlitePool) -> Result<(), StorageError> {
        MIGRATOR
            .run(pool)
            .await
            .map_err(|err| StorageError::Migration(err.to_string()))?;
        Ok(())
    }
}
