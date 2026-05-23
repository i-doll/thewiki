//! Postgres [`NamespaceRepository`](crate::repo::NamespaceRepository) impl.

use sqlx::PgPool;
use thewiki_core::{Namespace, NamespaceId, NamespaceSlug};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::StorageError;
use crate::postgres::codec::{is_fk_violation, map_unique_violation, namespace_from_row};
use crate::repo::NamespaceRepository;

/// Slug used for the implicit default namespace seeded at boot (#28).
const DEFAULT_NAMESPACE_SLUG: &str = "Main";

/// Slug used for the implicit template namespace seeded at boot (#45).
const TEMPLATE_NAMESPACE_SLUG: &str = "Template";

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

    async fn update_display_name(
        &self,
        id: NamespaceId,
        display_name: &str,
    ) -> Result<(), StorageError> {
        let result = sqlx::query("UPDATE namespaces SET display_name = $1 WHERE id = $2")
            .bind(display_name)
            .bind(id.into_uuid())
            .execute(self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    async fn delete(&self, id: NamespaceId) -> Result<(), StorageError> {
        let result = sqlx::query("DELETE FROM namespaces WHERE id = $1")
            .bind(id.into_uuid())
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
            Err(err) if is_fk_violation(&err) => Err(StorageError::Conflict(
                "namespace still contains pages".to_owned(),
            )),
            Err(err) => Err(StorageError::Database(err)),
        }
    }

    async fn get_or_create_default(&self) -> Result<Namespace, StorageError> {
        self.get_or_create_by_slug(DEFAULT_NAMESPACE_SLUG).await
    }

    async fn get_or_create_template_namespace(&self) -> Result<Namespace, StorageError> {
        self.get_or_create_by_slug(TEMPLATE_NAMESPACE_SLUG).await
    }
}

impl PostgresNamespaceRepository<'_> {
    /// Shared implementation of the idempotent "seed by slug" path used by
    /// [`get_or_create_default`](Self::get_or_create_default) and
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
                    Err(StorageError::Conflict(_)) => self.get_by_slug(&ns.slug).await,
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }
}
