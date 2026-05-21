//! SQLite [`NamespaceRepository`](crate::repo::NamespaceRepository) impl.

use sqlx::SqlitePool;
use thewiki_core::{Namespace, NamespaceId, NamespaceSlug};

use crate::error::StorageError;
use crate::repo::NamespaceRepository;
use crate::sqlite::codec::{format_ts, map_unique_violation, namespace_from_row, uuid_bytes};

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
}
