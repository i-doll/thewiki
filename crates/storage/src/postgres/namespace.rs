//! Postgres [`NamespaceRepository`](crate::repo::NamespaceRepository) impl.

use sqlx::PgPool;
use thewiki_core::{Namespace, NamespaceId, NamespaceSlug};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::StorageError;
use crate::postgres::codec::{map_unique_violation, namespace_from_row};
use crate::repo::NamespaceRepository;

/// Postgres-backed namespace repository. Borrows the pool from
/// [`PostgresStorage`](super::PostgresStorage).
pub struct PostgresNamespaceRepository<'a> {
    pool: &'a PgPool,
}

impl<'a> PostgresNamespaceRepository<'a> {
    pub(super) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }
}

impl NamespaceRepository for PostgresNamespaceRepository<'_> {
    async fn create(&self, namespace: &Namespace) -> Result<(), StorageError> {
        // The schema demands a `created_at`; the domain `Namespace` doesn't
        // expose one yet, so stamp "now" at insert time. When `Namespace`
        // grows a `created_at` field, swap this for the carried value.
        let now = OffsetDateTime::now_utc();

        let result = sqlx::query(
            "INSERT INTO namespaces (id, slug, display_name, created_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(namespace.id.into_uuid())
        .bind(namespace.slug.as_str())
        .bind(&namespace.display_name)
        .bind(now)
        .execute(self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(err, "namespace slug already in use")),
        }
    }

    async fn get_by_id(&self, id: NamespaceId) -> Result<Namespace, StorageError> {
        let row: Option<(Uuid, String, String)> =
            sqlx::query_as("SELECT id, slug, display_name FROM namespaces WHERE id = $1")
                .bind(id.into_uuid())
                .fetch_optional(self.pool)
                .await?;

        match row {
            Some((id, slug, display_name)) => namespace_from_row(id, slug, display_name),
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_slug(&self, slug: &NamespaceSlug) -> Result<Namespace, StorageError> {
        let row: Option<(Uuid, String, String)> =
            sqlx::query_as("SELECT id, slug, display_name FROM namespaces WHERE slug = $1")
                .bind(slug.as_str())
                .fetch_optional(self.pool)
                .await?;

        match row {
            Some((id, slug, display_name)) => namespace_from_row(id, slug, display_name),
            None => Err(StorageError::NotFound),
        }
    }

    async fn list(&self) -> Result<Vec<Namespace>, StorageError> {
        let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
            "SELECT id, slug, display_name FROM namespaces ORDER BY created_at ASC, id ASC",
        )
        .fetch_all(self.pool)
        .await?;

        rows.into_iter()
            .map(|(id, slug, display_name)| namespace_from_row(id, slug, display_name))
            .collect()
    }
}
