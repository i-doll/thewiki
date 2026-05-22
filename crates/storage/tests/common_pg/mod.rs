//! Shared helpers for the Postgres integration tests.
//!
//! The tests look for a Postgres instance via the `TEST_POSTGRES_URL` env
//! var, falling back to `DATABASE_URL`. If neither is set the helper emits
//! a one-line `eprintln!` and the calling test exits cleanly — running
//! `cargo test` on a developer machine without a Postgres available
//! shouldn't be a hard failure.
//!
//! Each test gets its own fresh database (`thewiki_test_<uuid>`) so they can
//! run in parallel without stomping each other's schema. The database is
//! dropped on `Drop` of [`FreshPg`].

#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(dead_code, reason = "shared helpers; each test uses a subset")]

use std::time::Duration;

use sqlx::{Executor, PgPool, postgres::PgPoolOptions};
use thewiki_core::{
    ContentFormat, EmailAddress, Namespace, NamespaceId, NamespaceSlug, Page, PageId, Permissions,
    ProtectionLevel, Role, RoleId, RoleName, User, UserId, Username,
};
use thewiki_storage::postgres::PostgresStorage;
use time::OffsetDateTime;
use uuid::Uuid;

/// Read the Postgres URL the integration tests should target.
///
/// `TEST_POSTGRES_URL` takes precedence so contributors can keep a regular
/// `DATABASE_URL` pointed at their local SQLite without those two collisions.
/// Returns `None` (and prints a guidance line) when neither is set.
#[must_use]
pub fn pg_admin_url() -> Option<String> {
    if let Ok(url) = std::env::var("TEST_POSTGRES_URL") {
        return Some(url);
    }
    if let Ok(url) = std::env::var("DATABASE_URL")
        && (url.starts_with("postgres://") || url.starts_with("postgresql://"))
    {
        return Some(url);
    }
    eprintln!(
        "[postgres test] skipping: set TEST_POSTGRES_URL or a postgres-flavoured DATABASE_URL to run"
    );
    None
}

/// Owns the per-test database and tears it down on drop.
///
/// The pool sitting on top of it is closed before the `DROP DATABASE` runs
/// so Postgres doesn't reject the drop with "other sessions are using the
/// database".
pub struct FreshPg {
    /// Pool against the freshly-created test database.
    pub pool: PgPool,
    /// libpq URL of that database (handy if a test needs a side pool).
    pub url: String,
    /// libpq URL of the admin database we connected to first (where
    /// `DROP DATABASE` runs).
    admin_url: String,
    /// Bookkeeping for the teardown.
    db_name: String,
}

impl FreshPg {
    async fn new() -> Option<Self> {
        let admin_url = pg_admin_url()?;
        // UUIDv7 prefix gives us a chronologically sortable suffix and the
        // workspace already enables the `v7` feature; we avoid pulling in
        // `v4` just for the test helper.
        let db_name = format!("thewiki_test_{}", Uuid::now_v7().simple());

        // Connect to the admin URL to issue `CREATE DATABASE`; that statement
        // can't run inside a transaction, so we use a one-shot connection.
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            .connect(&admin_url)
            .await
            .expect("connect to admin url");
        admin_pool
            .execute(format!("CREATE DATABASE {db_name}").as_str())
            .await
            .expect("create test database");
        admin_pool.close().await;

        let url = swap_database_in_url(&admin_url, &db_name);
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .connect(&url)
            .await
            .expect("connect to fresh test db");
        PostgresStorage::migrate(&pool)
            .await
            .expect("apply migrations to fresh test db");

        Some(Self {
            pool,
            url,
            admin_url,
            db_name,
        })
    }

    /// Drop the test database. Called from `drop`.
    fn teardown(admin_url: String, db_name: String) {
        // Tear down on a Tokio runtime — `drop` is sync. Spawning a fresh
        // single-threaded runtime keeps us off the executor that drove the
        // test itself (which may already be shutting down).
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build teardown runtime");
            rt.block_on(async move {
                let pool = match PgPoolOptions::new()
                    .max_connections(1)
                    .acquire_timeout(Duration::from_secs(5))
                    .connect(&admin_url)
                    .await
                {
                    Ok(p) => p,
                    Err(_) => return,
                };
                // Forcibly drop any leftover sessions before removing the db.
                let _ = pool
                    .execute(
                        format!(
                            "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                             WHERE datname = '{db_name}' AND pid <> pg_backend_pid()"
                        )
                        .as_str(),
                    )
                    .await;
                let _ = pool
                    .execute(format!("DROP DATABASE IF EXISTS {db_name}").as_str())
                    .await;
                pool.close().await;
            });
        })
        .join()
        .ok();
    }
}

impl Drop for FreshPg {
    fn drop(&mut self) {
        // `pg_terminate_backend` in `teardown` evicts any leftover sessions
        // from the test pool, so we don't need to close the pool first —
        // `Drop` on `PgPool` releases its handles when this struct goes away.
        Self::teardown(self.admin_url.clone(), self.db_name.clone());
    }
}

/// Boot a fresh Postgres database for the test, applying migrations.
///
/// Returns `None` when no Postgres URL is configured so the calling test can
/// skip with `return`. The returned [`FreshPg`] also drops the per-test
/// database when it goes out of scope; bind it to `_keep` (or similar) so it
/// stays alive for the full test body.
pub async fn fresh_pool() -> Option<FreshPg> {
    FreshPg::new().await
}

/// Boot a fresh Postgres-backed [`PostgresStorage`] for the test.
///
/// Returns the storage handle and the [`FreshPg`] teardown guard as a pair —
/// the caller must keep the guard alive for the duration of the test.
pub async fn fresh_storage() -> Option<(PostgresStorage, FreshPg)> {
    let fresh = FreshPg::new().await?;
    let storage = PostgresStorage::from_pool(fresh.pool.clone());
    Some((storage, fresh))
}

/// Swap the database segment of a libpq URL, preserving credentials and
/// query string. The admin URL `postgres://u:p@h:5432/postgres?x=1` becomes
/// `postgres://u:p@h:5432/<new>?x=1`.
fn swap_database_in_url(url: &str, new_db: &str) -> String {
    // `postgres://user:pass@host:port/dbname?query`
    let (scheme_authority, rest) = match url.find("://") {
        Some(i) => {
            let after_scheme = &url[i + 3..];
            (&url[..i + 3], after_scheme)
        }
        None => return format!("postgres://localhost/{new_db}"),
    };
    let (authority, path_and_query) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };
    let (_db, query) = match path_and_query.find('?') {
        Some(i) => (&path_and_query[..i], &path_and_query[i..]),
        None => (path_and_query, ""),
    };
    format!("{scheme_authority}{authority}/{new_db}{query}")
}

/// Build a [`Namespace`] with a deterministic slug and display name.
pub fn make_namespace(slug: &str) -> Namespace {
    Namespace {
        id: NamespaceId::new(),
        slug: NamespaceSlug::new(slug).expect("valid slug"),
        display_name: slug.to_string(),
    }
}

/// Build a [`User`] with the given username; everything else is filled in.
pub fn make_user(username: &str) -> User {
    User {
        id: UserId::new(),
        username: Username::new(username).expect("valid username"),
        email: Some(EmailAddress::new(format!("{username}@example.com")).expect("valid email")),
        display_name: Some(username.to_string()),
        created_at: OffsetDateTime::now_utc(),
        last_login_at: None,
    }
}

/// Build a [`Page`] in `namespace_id` with the given slug.
pub fn make_page(namespace_id: NamespaceId, slug: &str) -> Page {
    let now = OffsetDateTime::now_utc();
    Page {
        id: PageId::new(),
        namespace_id,
        slug: slug.to_string(),
        title: slug.to_string(),
        current_revision_id: None,
        content_format: ContentFormat::Markdown,
        protection_level: ProtectionLevel::None,
        created_at: now,
        updated_at: now,
    }
}

/// Build a [`Role`] with the given name + permissions.
pub fn make_role(name: &str, permissions: Permissions) -> Role {
    Role {
        id: RoleId::new(),
        name: RoleName::new(name).expect("valid role name"),
        display_name: name.to_string(),
        permissions,
    }
}
