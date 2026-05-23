//! Postgres [`NamespaceRepository`](crate::repo::NamespaceRepository) impl.

use sqlx::{PgPool, Postgres, Transaction};
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

/// Prefix prepended to a subject namespace slug to produce its discussion
/// counterpart (#43).
const TALK_SLUG_PREFIX: &str = "Talk_";

/// Prefix prepended to a subject namespace's display name to produce the
/// talk-side label.
const TALK_DISPLAY_PREFIX: &str = "Talk: ";

/// Postgres-backed namespace repository. Borrows the pool from
/// [`PostgresStorage`](super::PostgresStorage).
pub struct PostgresNamespaceRepository<'a> {
    pool: &'a PgPool,
}

impl<'a> PostgresNamespaceRepository<'a> {
    pub(super) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Build the matching talk-side namespace value for `subject`.
    fn build_talk_pair(subject: &Namespace) -> Result<Namespace, StorageError> {
        let talk_slug = format!("{TALK_SLUG_PREFIX}{}", subject.slug.as_str());
        let slug = NamespaceSlug::new(&talk_slug).map_err(|err| {
            StorageError::invalid_input(format!(
                "could not derive talk slug from {:?}: {err}",
                subject.slug.as_str()
            ))
        })?;
        Ok(Namespace {
            id: NamespaceId::new(),
            slug,
            display_name: format!("{TALK_DISPLAY_PREFIX}{}", subject.display_name),
            is_talk: true,
            paired_namespace_id: Some(subject.id),
        })
    }

    async fn insert_row_tx(
        tx: &mut Transaction<'_, Postgres>,
        namespace: &Namespace,
    ) -> Result<(), StorageError> {
        let now = OffsetDateTime::now_utc();
        let paired = namespace.paired_namespace_id.map(|p| p.into_uuid());

        let result = sqlx::query(
            "INSERT INTO namespaces (id, slug, display_name, created_at, is_talk, paired_namespace_id) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(namespace.id.into_uuid())
        .bind(namespace.slug.as_str())
        .bind(&namespace.display_name)
        .bind(now)
        .bind(namespace.is_talk)
        .bind(paired)
        .execute(&mut **tx)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(err, "namespace slug already in use")),
        }
    }

    async fn set_pair_tx(
        tx: &mut Transaction<'_, Postgres>,
        id: NamespaceId,
        paired_id: NamespaceId,
    ) -> Result<(), StorageError> {
        sqlx::query("UPDATE namespaces SET paired_namespace_id = $1 WHERE id = $2")
            .bind(paired_id.into_uuid())
            .bind(id.into_uuid())
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// Lookup a namespace by slug inside the current transaction. Used by
    /// `create()` so the "Talk_<slug> already exists" branch reads the
    /// same view of the data the surrounding writes do.
    async fn get_by_slug_tx(
        tx: &mut Transaction<'_, Postgres>,
        slug: &NamespaceSlug,
    ) -> Result<Namespace, StorageError> {
        let row: Option<(Uuid, String, String, bool, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, slug, display_name, is_talk, paired_namespace_id FROM namespaces WHERE slug = $1",
        )
        .bind(slug.as_str())
        .fetch_optional(&mut **tx)
        .await?;

        match row {
            Some((id, slug, display_name, is_talk, paired)) => {
                namespace_from_row(id, slug, display_name, is_talk, paired)
            }
            None => Err(StorageError::NotFound),
        }
    }
}

impl NamespaceRepository for PostgresNamespaceRepository<'_> {
    async fn create(&self, namespace: &Namespace) -> Result<(), StorageError> {
        // The subject insert, the paired talk insert, and the bidirectional
        // `paired_namespace_id` updates all run inside a single transaction
        // so a failure on any later step rolls back the whole pairing
        // graph (#43, coderabbit).
        let mut tx = self.pool.begin().await?;
        Self::insert_row_tx(&mut tx, namespace).await?;

        if !namespace.is_talk && namespace.paired_namespace_id.is_none() {
            let talk = Self::build_talk_pair(namespace)?;
            match Self::insert_row_tx(&mut tx, &talk).await {
                Ok(()) => {
                    Self::set_pair_tx(&mut tx, namespace.id, talk.id).await?;
                }
                Err(StorageError::Conflict(_)) => {
                    let existing = Self::get_by_slug_tx(&mut tx, &talk.slug).await?;
                    if existing.paired_namespace_id.is_none() {
                        Self::set_pair_tx(&mut tx, existing.id, namespace.id).await?;
                    }
                    Self::set_pair_tx(&mut tx, namespace.id, existing.id).await?;
                }
                Err(e) => return Err(e),
            }
        }
        tx.commit().await?;
        Ok(())
    }

    async fn get_by_id(&self, id: NamespaceId) -> Result<Namespace, StorageError> {
        let row: Option<(Uuid, String, String, bool, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, slug, display_name, is_talk, paired_namespace_id FROM namespaces WHERE id = $1",
        )
        .bind(id.into_uuid())
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some((id, slug, display_name, is_talk, paired)) => {
                namespace_from_row(id, slug, display_name, is_talk, paired)
            }
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_slug(&self, slug: &NamespaceSlug) -> Result<Namespace, StorageError> {
        let row: Option<(Uuid, String, String, bool, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, slug, display_name, is_talk, paired_namespace_id FROM namespaces WHERE slug = $1",
        )
        .bind(slug.as_str())
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some((id, slug, display_name, is_talk, paired)) => {
                namespace_from_row(id, slug, display_name, is_talk, paired)
            }
            None => Err(StorageError::NotFound),
        }
    }

    async fn list(&self) -> Result<Vec<Namespace>, StorageError> {
        let rows: Vec<(Uuid, String, String, bool, Option<Uuid>)> = sqlx::query_as(
            "SELECT id, slug, display_name, is_talk, paired_namespace_id FROM namespaces ORDER BY created_at ASC, id ASC",
        )
        .fetch_all(self.pool)
        .await?;

        rows.into_iter()
            .map(|(id, slug, display_name, is_talk, paired)| {
                namespace_from_row(id, slug, display_name, is_talk, paired)
            })
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
                    is_talk: false,
                    paired_namespace_id: None,
                };
                match self.create(&ns).await {
                    Ok(()) => self.get_by_id(ns.id).await,
                    Err(StorageError::Conflict(_)) => self.get_by_slug(&ns.slug).await,
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }
}
