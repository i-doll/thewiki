//! Postgres-backed implementations of the [`Repository`](crate::repo) traits.
//!
//! The entry point is [`PostgresStorage`], which owns a [`sqlx::PgPool`] and
//! exposes one repository handle per aggregate via accessor methods. The shape
//! mirrors [`crate::sqlite::SqliteStorage`] so call sites can stay generic
//! over the trait surface.
//!
//! ## Pool & options
//!
//! Connection pooling is configured through [`PostgresOptions`]:
//!
//! ```no_run
//! # async fn doc() -> Result<(), thewiki_storage::StorageError> {
//! use thewiki_storage::postgres::{PostgresOptions, PostgresStorage};
//! use std::time::Duration;
//!
//! let storage = PostgresStorage::new(
//!     "postgres://user:pass@localhost/thewiki",
//!     PostgresOptions {
//!         max_connections: 10,
//!         acquire_timeout: Duration::from_secs(30),
//!     },
//! )
//! .await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Type mappings
//!
//! | Domain                 | Column        | Encoding                               |
//! |------------------------|---------------|----------------------------------------|
//! | `*Id` (UUIDv7)         | `UUID`        | native `uuid::Uuid`                    |
//! | `OffsetDateTime`       | `TIMESTAMPTZ` | native via the sqlx `time` feature     |
//! | `Permissions` (u32)    | `BIGINT`      | `bits() as i64` (widening)             |
//! | `ContentFormat`        | `TEXT`        | `as_str()`                             |
//! | `ProtectionLevel`      | `TEXT`        | `as_str()`                             |
//! | audit `metadata`       | `JSONB`       | `serde_json::Value`                    |
//!
//! ## Migrations
//!
//! Postgres-flavoured migrations live under `migrations/postgres/`. They are
//! baked in at build time via `sqlx::migrate!` so the binary can apply them
//! without shipping the SQL files separately.

use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

use crate::error::StorageError;

mod audit_log;
mod codec;
mod media;
mod namespace;
mod page;
mod recent_changes;
mod revision;
mod role;
mod session;
mod user;

pub use audit_log::PostgresAuditLogRepository;
pub use media::{
    PostgresMediaBlobRepository, PostgresMediaRepository, PostgresMediaVariantRepository,
};
pub use namespace::PostgresNamespaceRepository;
pub use page::PostgresPageRepository;
pub use recent_changes::PostgresRecentChangesRepository;
pub use revision::PostgresRevisionRepository;
pub use role::PostgresRoleRepository;
pub use session::PostgresSessionRepository;
pub use user::PostgresUserRepository;

/// Postgres-flavoured migration set, loaded from `migrations/postgres/`.
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations/postgres");

/// Tuning knobs for the [`PgPool`] backing [`PostgresStorage`].
#[derive(Debug, Clone)]
pub struct PostgresOptions {
    /// Maximum number of connections the pool will hold open.
    pub max_connections: u32,
    /// How long a caller waits for a free connection before `acquire` errors.
    pub acquire_timeout: Duration,
}

impl Default for PostgresOptions {
    fn default() -> Self {
        Self {
            max_connections: 10,
            acquire_timeout: Duration::from_secs(30),
        }
    }
}

/// Postgres-backed storage facade.
///
/// Holds the pool and dispenses per-aggregate repository handles. Clone is
/// cheap — the inner `PgPool` is already an `Arc`.
#[derive(Debug, Clone)]
pub struct PostgresStorage {
    pool: PgPool,
}

impl PostgresStorage {
    /// Open a pool against `url`, apply migrations, and return a handle.
    ///
    /// `url` is a libpq-style connection string
    /// (`postgres://user:pass@host:port/database?sslmode=require`).
    ///
    /// # Errors
    ///
    /// * [`StorageError::Database`] on pool / driver failures.
    /// * [`StorageError::Migration`] if the migration set fails to apply.
    pub async fn new(url: &str, opts: PostgresOptions) -> Result<Self, StorageError> {
        let pool = PgPoolOptions::new()
            .max_connections(opts.max_connections)
            .acquire_timeout(opts.acquire_timeout)
            .connect(url)
            .await?;

        MIGRATOR
            .run(&pool)
            .await
            .map_err(|err| StorageError::Migration(err.to_string()))?;

        Ok(Self { pool })
    }

    /// Construct a [`PostgresStorage`] around an already-built pool.
    ///
    /// Migrations are **not** run — the caller is responsible for getting the
    /// schema into place (typically via [`Self::migrate`]).
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Borrow the underlying pool.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Borrow this handle as a [`PageRepository`](crate::repo::PageRepository).
    #[must_use]
    pub fn pages(&self) -> PostgresPageRepository<'_> {
        PostgresPageRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`RevisionRepository`](crate::repo::RevisionRepository).
    #[must_use]
    pub fn revisions(&self) -> PostgresRevisionRepository<'_> {
        PostgresRevisionRepository::new(&self.pool)
    }

    /// Borrow this handle as a [`UserRepository`](crate::repo::UserRepository).
    #[must_use]
    pub fn users(&self) -> PostgresUserRepository<'_> {
        PostgresUserRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`NamespaceRepository`](crate::repo::NamespaceRepository).
    #[must_use]
    pub fn namespaces(&self) -> PostgresNamespaceRepository<'_> {
        PostgresNamespaceRepository::new(&self.pool)
    }

    /// Borrow this handle as a [`RoleRepository`](crate::repo::RoleRepository).
    #[must_use]
    pub fn roles(&self) -> PostgresRoleRepository<'_> {
        PostgresRoleRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`SessionRepository`](crate::repo::SessionRepository).
    #[must_use]
    pub fn sessions(&self) -> PostgresSessionRepository<'_> {
        PostgresSessionRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`RecentChangesRepository`](crate::repo::RecentChangesRepository).
    #[must_use]
    pub fn recent_changes(&self) -> PostgresRecentChangesRepository<'_> {
        PostgresRecentChangesRepository::new(&self.pool)
    }

    /// Borrow this handle as an [`AuditLogRepository`](crate::repo::AuditLogRepository).
    #[must_use]
    pub fn audit_log(&self) -> PostgresAuditLogRepository<'_> {
        PostgresAuditLogRepository::new(&self.pool)
    }

    /// Borrow this handle as a [`MediaRepository`](crate::repo::MediaRepository).
    #[must_use]
    pub fn media(&self) -> PostgresMediaRepository<'_> {
        PostgresMediaRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`MediaBlobRepository`](crate::repo::MediaBlobRepository). Only
    /// useful when the configured storage backend is `Db`.
    #[must_use]
    pub fn media_blobs(&self) -> PostgresMediaBlobRepository<'_> {
        PostgresMediaBlobRepository::new(&self.pool)
    }

    /// Borrow this handle as a
    /// [`MediaVariantRepository`](crate::repo::MediaVariantRepository) (#33).
    #[must_use]
    pub fn media_variants(&self) -> PostgresMediaVariantRepository<'_> {
        PostgresMediaVariantRepository::new(&self.pool)
    }

    /// Apply the embedded Postgres migration set to an arbitrary pool.
    ///
    /// Exposed for [`from_pool`](Self::from_pool) callers and integration
    /// tests that build their own pool.
    ///
    /// # Errors
    ///
    /// [`StorageError::Migration`] if any migration fails.
    pub async fn migrate(pool: &PgPool) -> Result<(), StorageError> {
        MIGRATOR
            .run(pool)
            .await
            .map_err(|err| StorageError::Migration(err.to_string()))?;
        Ok(())
    }
}
