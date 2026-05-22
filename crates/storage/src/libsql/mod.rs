//! libsql / Turso-backed implementations of the [`Repository`](crate::repo)
//! traits.
//!
//! libsql is Turso's fork of SQLite: identical SQL dialect, identical on-disk
//! format, but a different Rust client with native async, embedded replicas,
//! and a remote-over-HTTP transport. The schema in `/migrations/` is portable
//! and runs unmodified on both engines, so this adapter mirrors the SQLite
//! one query-for-query — only the driver call sites differ.
//!
//! ## Opening a storage handle
//!
//! Three modes, picked at construction time via [`LibsqlOptions`]:
//!
//! ```no_run
//! # async fn doc() -> Result<(), thewiki_storage::StorageError> {
//! use thewiki_storage::libsql::{LibsqlOptions, LibsqlStorage};
//!
//! // In-memory (great for tests):
//! let storage = LibsqlStorage::new(LibsqlOptions::in_memory()).await?;
//!
//! // Local file (single-node deployments):
//! let storage = LibsqlStorage::new(LibsqlOptions::local("/var/lib/thewiki.db")).await?;
//!
//! // Remote Turso instance:
//! let storage = LibsqlStorage::new(
//!     LibsqlOptions::remote("libsql://example.turso.io", "auth-token-here"),
//! )
//! .await?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Migrations
//!
//! libsql does not ship a sqlx-style migration runner. Instead we bake the
//! migration files into the binary at compile time via [`MIGRATIONS`] and apply
//! them in lexicographic order from [`LibsqlStorage::new`], tracking which
//! files have already run in a `_libsql_migrations` table. This mirrors the
//! semantics of `sqlx::migrate!` — forward-only, name-ordered, idempotent —
//! while staying inside libsql's own client API as the issue requires.
//!
//! ## Connection model
//!
//! `libsql::Connection` is a cheap `Arc`-backed handle that's internally
//! serialised. We hold one connection per [`LibsqlStorage`] and clone it for
//! the per-aggregate repository handles. For `:memory:` databases this is the
//! only thing that works (each `Database::connect` call would otherwise hand
//! out a fresh, empty memory image); for file-backed and remote databases it
//! keeps the driver's own concurrency control as the single source of truth.
//!
//! ## Type mappings
//!
//! Same as the SQLite adapter — see [`crate::sqlite`] for the table.

#![allow(
    clippy::missing_errors_doc,
    reason = "Repository trait already documents the error contract per method."
)]

use ::libsql::{Builder, Connection};

use crate::error::StorageError;

mod audit_log;
mod codec;
mod namespace;
mod page;
mod page_audit;
mod recent_changes;
mod revision;
mod role;
mod session;
mod user;

pub use audit_log::LibsqlAuditLogRepository;
pub use namespace::LibsqlNamespaceRepository;
pub use page::LibsqlPageRepository;
pub use recent_changes::LibsqlRecentChangesRepository;
pub use revision::LibsqlRevisionRepository;
pub use role::LibsqlRoleRepository;
pub use session::LibsqlSessionRepository;
pub use user::LibsqlUserRepository;

/// Embedded migration set, applied in the order listed by
/// [`LibsqlStorage::new`].
///
/// Each entry is `(name, sql)`. `name` is the filename without the `.sql`
/// suffix, matched against the `_libsql_migrations.name` column to make the
/// runner idempotent. Keep this list in lexicographic order — the runner
/// preserves the listed order rather than re-sorting.
///
/// Adding a new migration means dropping a new `.sql` file under
/// `/migrations/` *and* a new entry here. The duplication is intentional: we'd
/// rather have the compiler tell us "you forgot to wire it in" than walk a
/// directory at runtime and silently miss a file in a release binary.
const MIGRATIONS: &[(&str, &str)] = &[
    (
        "00000000000000_init",
        include_str!("../../../../migrations/00000000000000_init.sql"),
    ),
    (
        "20260522005020_sessions",
        include_str!("../../../../migrations/20260522005020_sessions.sql"),
    ),
    (
        "20260522093000_audit_log",
        include_str!("../../../../migrations/20260522093000_audit_log.sql"),
    ),
];

/// Construction options for [`LibsqlStorage`].
///
/// Three constructors cover the practical deployment modes. Build one with the
/// associated function and hand it to [`LibsqlStorage::new`].
#[derive(Debug, Clone)]
pub struct LibsqlOptions {
    mode: Mode,
}

#[derive(Debug, Clone)]
enum Mode {
    /// Fresh in-memory database. Forgotten on drop.
    InMemory,
    /// Local file path. Created if missing.
    Local { path: String },
    /// Remote Turso instance.
    Remote { url: String, auth_token: String },
}

impl LibsqlOptions {
    /// Open a fresh in-memory libsql database.
    ///
    /// Useful for integration tests; the database is wiped when the
    /// [`LibsqlStorage`] handle is dropped.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            mode: Mode::InMemory,
        }
    }

    /// Open a local file-backed libsql database at `path`.
    ///
    /// The file is created if it doesn't exist. Same semantics as a vanilla
    /// SQLite database on disk.
    #[must_use]
    pub fn local(path: impl Into<String>) -> Self {
        Self {
            mode: Mode::Local { path: path.into() },
        }
    }

    /// Connect to a remote Turso instance at `url` using `auth_token`.
    ///
    /// `url` typically starts with `libsql://` or `https://`. All queries are
    /// proxied over the Hrana HTTP protocol — no local database file is
    /// created.
    #[must_use]
    pub fn remote(url: impl Into<String>, auth_token: impl Into<String>) -> Self {
        Self {
            mode: Mode::Remote {
                url: url.into(),
                auth_token: auth_token.into(),
            },
        }
    }
}

/// libsql / Turso-backed storage facade.
///
/// Holds a single shared connection and hands out per-aggregate repository
/// handles. Clone is cheap — the inner `Connection` is already `Arc`-backed.
#[derive(Debug, Clone)]
pub struct LibsqlStorage {
    conn: Connection,
}

impl LibsqlStorage {
    /// Open a libsql connection, enable foreign-key enforcement, and apply
    /// the embedded migration set.
    ///
    /// # Errors
    ///
    /// * [`StorageError::Database`] on connection / driver failures.
    /// * [`StorageError::Migration`] if any migration fails to apply.
    pub async fn new(opts: LibsqlOptions) -> Result<Self, StorageError> {
        let db = match opts.mode {
            Mode::InMemory => Builder::new_local(":memory:")
                .build()
                .await
                .map_err(codec::db_error)?,
            Mode::Local { path } => Builder::new_local(path)
                .build()
                .await
                .map_err(codec::db_error)?,
            Mode::Remote { url, auth_token } => Builder::new_remote(url, auth_token)
                .build()
                .await
                .map_err(codec::db_error)?,
        };

        let conn = db.connect().map_err(codec::db_error)?;

        // libsql, like SQLite, doesn't enforce FKs unless asked. Our schema
        // relies on cascades (`revisions` follows `pages`, `sessions` follows
        // `users`), so opt in unconditionally. On a remote connection this is
        // a no-op handled server-side.
        codec::into_db(conn.execute("PRAGMA foreign_keys = ON", ()).await)?;

        run_migrations(&conn).await?;

        Ok(Self { conn })
    }

    /// Construct a storage handle around an already-open `libsql::Connection`.
    ///
    /// Migrations are **not** run — the caller is responsible for getting the
    /// schema into place. Useful for tests that want to share a connection
    /// across helpers or for callers that need finer-grained control than
    /// [`LibsqlOptions`] exposes.
    #[must_use]
    pub fn from_connection(conn: Connection) -> Self {
        Self { conn }
    }

    /// Borrow the underlying connection (for transactional use cases that span
    /// multiple repositories).
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Borrow this handle as a [`PageRepository`](crate::repo::PageRepository).
    #[must_use]
    pub fn pages(&self) -> LibsqlPageRepository<'_> {
        LibsqlPageRepository::new(&self.conn)
    }

    /// Borrow this handle as a
    /// [`RevisionRepository`](crate::repo::RevisionRepository).
    #[must_use]
    pub fn revisions(&self) -> LibsqlRevisionRepository<'_> {
        LibsqlRevisionRepository::new(&self.conn)
    }

    /// Borrow this handle as a [`UserRepository`](crate::repo::UserRepository).
    #[must_use]
    pub fn users(&self) -> LibsqlUserRepository<'_> {
        LibsqlUserRepository::new(&self.conn)
    }

    /// Borrow this handle as a
    /// [`NamespaceRepository`](crate::repo::NamespaceRepository).
    #[must_use]
    pub fn namespaces(&self) -> LibsqlNamespaceRepository<'_> {
        LibsqlNamespaceRepository::new(&self.conn)
    }

    /// Borrow this handle as a [`RoleRepository`](crate::repo::RoleRepository).
    #[must_use]
    pub fn roles(&self) -> LibsqlRoleRepository<'_> {
        LibsqlRoleRepository::new(&self.conn)
    }

    /// Borrow this handle as a
    /// [`SessionRepository`](crate::repo::SessionRepository).
    #[must_use]
    pub fn sessions(&self) -> LibsqlSessionRepository<'_> {
        LibsqlSessionRepository::new(&self.conn)
    }

    /// Borrow this handle as a
    /// [`RecentChangesRepository`](crate::repo::RecentChangesRepository).
    #[must_use]
    pub fn recent_changes(&self) -> LibsqlRecentChangesRepository<'_> {
        LibsqlRecentChangesRepository::new(&self.conn)
    }

    /// Borrow this handle as an
    /// [`AuditLogRepository`](crate::repo::AuditLogRepository).
    #[must_use]
    pub fn audit_log(&self) -> LibsqlAuditLogRepository<'_> {
        LibsqlAuditLogRepository::new(&self.conn)
    }

    /// Commit a page mutation together with its audit-log row.
    ///
    /// The operation runs in a single libsql transaction so a successful page
    /// mutation cannot be reported without a matching audit entry.
    pub async fn commit_page_audit(
        &self,
        mutation: crate::repo::PageAuditMutation,
        audit: crate::repo::NewAuditLogEntry,
    ) -> Result<(), StorageError> {
        page_audit::commit_page_audit(&self.conn, mutation, audit).await
    }

    /// Apply the embedded migration set to an arbitrary connection.
    ///
    /// Exposed so callers that built their own [`Connection`] via
    /// [`from_connection`](Self::from_connection) can still bring it up to the
    /// expected schema.
    ///
    /// # Errors
    ///
    /// [`StorageError::Migration`] if any migration fails.
    pub async fn migrate(conn: &Connection) -> Result<(), StorageError> {
        run_migrations(conn).await
    }
}

/// Apply [`MIGRATIONS`] in order, skipping any that have already been recorded
/// in `_libsql_migrations`.
///
/// We deliberately wrap each migration in its own transaction rather than one
/// big transactional batch: the inaugural migration creates `pages` and
/// `revisions`, which forward-reference each other, and bundling subsequent
/// migrations in the same transaction would prevent newer files from seeing
/// schema changes made by earlier ones in some libsql builds. One-by-one keeps
/// the semantics matching `sqlx::migrate!`.
async fn run_migrations(conn: &Connection) -> Result<(), StorageError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _libsql_migrations (
             name       TEXT PRIMARY KEY NOT NULL,
             applied_at TEXT NOT NULL
         )",
    )
    .await
    .map_err(|err| StorageError::Migration(err.to_string()))?;

    for (name, sql) in MIGRATIONS {
        if migration_applied(conn, name).await? {
            continue;
        }
        // `execute_batch` runs every statement in the file; many of our
        // migrations are multi-statement (CREATE TABLE + indexes). It is *not*
        // wrapped in a transaction by libsql, so we wrap it ourselves so a
        // half-applied migration doesn't leave the schema in a torn state.
        conn.execute_batch("BEGIN")
            .await
            .map_err(|err| StorageError::Migration(format!("{name}: begin: {err}")))?;
        if let Err(err) = conn.execute_batch(sql).await {
            // Best-effort rollback — if it also fails we still want to report
            // the original error, not the rollback's.
            let _ = conn.execute_batch("ROLLBACK").await;
            return Err(StorageError::Migration(format!("{name}: {err}")));
        }
        let now = time::OffsetDateTime::now_utc();
        let now_str = crate::codec::format_ts(now)?;
        conn.execute(
            "INSERT INTO _libsql_migrations (name, applied_at) VALUES (?1, ?2)",
            (*name, now_str),
        )
        .await
        .map_err(|err| StorageError::Migration(format!("{name}: record: {err}")))?;
        conn.execute_batch("COMMIT")
            .await
            .map_err(|err| StorageError::Migration(format!("{name}: commit: {err}")))?;
    }

    Ok(())
}

async fn migration_applied(conn: &Connection, name: &str) -> Result<bool, StorageError> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM _libsql_migrations WHERE name = ?1",
            [name.to_owned()],
        )
        .await
        .map_err(|err| StorageError::Migration(err.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|err| StorageError::Migration(err.to_string()))?;
    Ok(row.is_some())
}
