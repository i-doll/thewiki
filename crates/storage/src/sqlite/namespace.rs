//! SQLite [`NamespaceRepository`](crate::repo::NamespaceRepository) impl.

use sqlx::SqlitePool;
use thewiki_core::{Namespace, NamespaceId, NamespaceSlug};

use crate::error::StorageError;
use crate::repo::NamespaceRepository;
use crate::sqlite::codec::{format_ts, map_unique_violation, namespace_from_row, uuid_bytes};

/// Slug used for the implicit default namespace seeded at boot (#28).
const DEFAULT_NAMESPACE_SLUG: &str = "Main";

/// Slug used for the implicit template namespace seeded at boot (#45).
const TEMPLATE_NAMESPACE_SLUG: &str = "Template";

/// SQLite-backed namespace repository. Borrows the pool from
/// [`SqliteStorage`](super::SqliteStorage).
pub struct SqliteNamespaceRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteNamespaceRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl NamespaceRepository for SqliteNamespaceRepository<'_> {
    async fn create(&self, namespace: &Namespace) -> Result<(), StorageError> {
        // The schema demands a `created_at`; the domain `Namespace` doesn't
        // expose one yet, so stamp "now" at insert time. When `Namespace`
        // grows a `created_at` field, swap this for the carried value.
        let now = format_ts(time::OffsetDateTime::now_utc())?;
        let id = uuid_bytes(namespace.id.into_uuid());

        let result = sqlx::query(
            "INSERT INTO namespaces (id, slug, display_name, created_at) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(id.as_slice())
        .bind(namespace.slug.as_str())
        .bind(&namespace.display_name)
        .bind(&now)
        .execute(self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(err, "namespace slug already in use")),
        }
    }

    async fn get_by_id(&self, id: NamespaceId) -> Result<Namespace, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let row: Option<(Vec<u8>, String, String, String)> = sqlx::query_as(
            "SELECT id, slug, display_name, created_at FROM namespaces WHERE id = ?1",
        )
        .bind(id_bytes.as_slice())
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some((id, slug, display_name, created_at)) => {
                namespace_from_row(id, slug, display_name, created_at)
            }
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_slug(&self, slug: &NamespaceSlug) -> Result<Namespace, StorageError> {
        let row: Option<(Vec<u8>, String, String, String)> = sqlx::query_as(
            "SELECT id, slug, display_name, created_at FROM namespaces WHERE slug = ?1",
        )
        .bind(slug.as_str())
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some((id, slug, display_name, created_at)) => {
                namespace_from_row(id, slug, display_name, created_at)
            }
            None => Err(StorageError::NotFound),
        }
    }

    async fn list(&self) -> Result<Vec<Namespace>, StorageError> {
        let rows: Vec<(Vec<u8>, String, String, String)> = sqlx::query_as(
            "SELECT id, slug, display_name, created_at FROM namespaces ORDER BY created_at ASC, id ASC",
        )
        .fetch_all(self.pool)
        .await?;

        rows.into_iter()
            .map(|(id, slug, display_name, created_at)| {
                namespace_from_row(id, slug, display_name, created_at)
            })
            .collect()
    }

    async fn update_display_name(
        &self,
        id: NamespaceId,
        display_name: &str,
    ) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let result = sqlx::query("UPDATE namespaces SET display_name = ?1 WHERE id = ?2")
            .bind(display_name)
            .bind(id_bytes.as_slice())
            .execute(self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    async fn delete(&self, id: NamespaceId) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let result = sqlx::query("DELETE FROM namespaces WHERE id = ?1")
            .bind(id_bytes.as_slice())
            .execute(self.pool)
            .await;
        match result {
            Ok(res) => {
                if res.rows_affected() == 0 {
                    Err(StorageError::NotFound)
                } else {
                    Ok(())
                }
            }
            // The FK from `pages.namespace_id` is `ON DELETE RESTRICT`, so a
            // non-empty namespace surfaces as a foreign-key violation. Map
            // it to `Conflict` so the API layer can return 409 with a clear
            // "move the pages first" message.
            Err(err) => Err(map_fk_violation_as_conflict(
                err,
                "namespace still contains pages",
            )),
        }
    }

    async fn get_or_create_default(&self) -> Result<Namespace, StorageError> {
        self.get_or_create_by_slug(DEFAULT_NAMESPACE_SLUG).await
    }

    async fn get_or_create_template_namespace(&self) -> Result<Namespace, StorageError> {
        self.get_or_create_by_slug(TEMPLATE_NAMESPACE_SLUG).await
    }
}

impl SqliteNamespaceRepository<'_> {
    /// Shared implementation of the idempotent "seed by slug" path used by
    /// both [`get_or_create_default`](Self::get_or_create_default) and
    /// [`get_or_create_template_namespace`](Self::get_or_create_template_namespace).
    async fn get_or_create_by_slug(&self, slug_str: &str) -> Result<Namespace, StorageError> {
        let slug = NamespaceSlug::new(slug_str).map_err(|e| {
            StorageError::InvalidInput(format!("namespace slug {slug_str:?} is invalid: {e}"))
        })?;
        match self.get_by_slug(&slug).await {
            Ok(ns) => Ok(ns),
            Err(StorageError::NotFound) => {
                let ns = Namespace {
                    id: NamespaceId::new(),
                    slug,
                    display_name: slug_str.to_owned(),
                };
                match self.create(&ns).await {
                    Ok(()) => Ok(ns),
                    // A racing caller beat us — fetch the now-existing row.
                    Err(StorageError::Conflict(_)) => self.get_by_slug(&ns.slug).await,
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }
}

/// SQLite's foreign-key violation surfaces as extended error code `787`
/// (`SQLITE_CONSTRAINT_FOREIGNKEY`). Map it to [`StorageError::Conflict`]
/// so the API layer can render a 409 with a "move the pages first" message
/// when the operator tries to delete a non-empty namespace.
fn map_fk_violation_as_conflict(err: sqlx::Error, message: &str) -> StorageError {
    if let Some(db_err) = err.as_database_error()
        && db_err.code().as_deref() == Some("787")
    {
        return StorageError::Conflict(message.to_owned());
    }
    StorageError::from(err)
}
