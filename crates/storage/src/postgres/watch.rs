//! Postgres [`WatchRepository`](crate::repo::WatchRepository) impl (#46).

use sqlx::PgPool;
use thewiki_core::{NamespaceId, PageId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::StorageError;
use crate::postgres::codec::parse_protection_level;
use crate::repo::{WatchRepository, WatchedPage, clamp_limit};

type WatchJoinRow = (
    Uuid,           // pages.id
    Uuid,           // pages.namespace_id
    String,         // namespaces.slug
    String,         // pages.slug
    String,         // pages.title
    String,         // pages.protection_level
    OffsetDateTime, // watch.created_at
    OffsetDateTime, // pages.updated_at
);

/// Postgres-backed watchlist repository.
pub struct PostgresWatchRepository<'a> {
    pool: &'a PgPool,
}

impl<'a> PostgresWatchRepository<'a> {
    pub(super) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }
}

impl WatchRepository for PostgresWatchRepository<'_> {
    async fn watch(&self, user_id: UserId, page_id: PageId) -> Result<(), StorageError> {
        // `ON CONFLICT DO NOTHING` keeps the original `created_at` if the row
        // is already there — re-watching is idempotent.
        sqlx::query(
            "INSERT INTO watch (user_id, page_id, created_at)
             VALUES ($1, $2, $3)
             ON CONFLICT (user_id, page_id) DO NOTHING",
        )
        .bind(user_id.into_uuid())
        .bind(page_id.into_uuid())
        .bind(OffsetDateTime::now_utc())
        .execute(self.pool)
        .await?;
        Ok(())
    }

    async fn unwatch(&self, user_id: UserId, page_id: PageId) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM watch WHERE user_id = $1 AND page_id = $2")
            .bind(user_id.into_uuid())
            .bind(page_id.into_uuid())
            .execute(self.pool)
            .await?;
        Ok(())
    }

    async fn is_watched(&self, user_id: UserId, page_id: PageId) -> Result<bool, StorageError> {
        let row: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM watch WHERE user_id = $1 AND page_id = $2 LIMIT 1")
                .bind(user_id.into_uuid())
                .bind(page_id.into_uuid())
                .fetch_optional(self.pool)
                .await?;
        Ok(row.is_some())
    }

    async fn list_for_user(
        &self,
        user_id: UserId,
        limit: u32,
    ) -> Result<Vec<WatchedPage>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit);

        let rows: Vec<WatchJoinRow> = sqlx::query_as(
            "SELECT pages.id, pages.namespace_id, namespaces.slug,
                    pages.slug, pages.title, pages.protection_level,
                    watch.created_at, pages.updated_at
             FROM watch
             JOIN pages      ON pages.id      = watch.page_id
             JOIN namespaces ON namespaces.id = pages.namespace_id
             WHERE watch.user_id = $1
             ORDER BY watch.created_at DESC, watch.page_id DESC
             LIMIT $2",
        )
        .bind(user_id.into_uuid())
        .bind(take)
        .fetch_all(self.pool)
        .await?;

        rows.into_iter()
            .map(
                |(id, ns_id, ns_slug, page_slug, title, prot, watched_at, updated_at)| {
                    Ok(WatchedPage {
                        page_id: PageId::from_uuid(id),
                        namespace_id: NamespaceId::from_uuid(ns_id),
                        namespace_slug: ns_slug,
                        page_slug,
                        page_title: title,
                        protection_level: parse_protection_level(&prot)?,
                        watched_at,
                        updated_at,
                    })
                },
            )
            .collect()
    }
}
