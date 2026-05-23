//! SQLite [`WatchRepository`](crate::repo::WatchRepository) impl (#46).
//!
//! Backs the per-user watchlist feature. The table is intentionally tiny —
//! `(user_id, page_id, created_at)` with a composite primary key — and the
//! interesting query is the read-side JOIN that hydrates `(namespace_slug,
//! page_slug, title, protection_level, updated_at)` so the API and the
//! Atom feed can render rows in one shot.

use sqlx::SqlitePool;
use thewiki_core::{NamespaceId, PageId, UserId};

use crate::codec::parse_protection_level;
use crate::error::StorageError;
use crate::repo::{WatchRepository, WatchedPage, clamp_limit};
use crate::sqlite::codec::{decode_uuid, format_ts, parse_ts, uuid_bytes};
use time::OffsetDateTime;

/// Shape of one `list_for_user` JOIN row.
///
/// Pulled out as a `type` alias so clippy's complexity lint stays quiet.
type WatchJoinRow = (
    Vec<u8>, // pages.id
    Vec<u8>, // pages.namespace_id
    String,  // namespaces.slug
    String,  // pages.slug
    String,  // pages.title
    String,  // pages.protection_level
    String,  // watch.created_at
    String,  // pages.updated_at
);

/// SQLite-backed watchlist repository.
pub struct SqliteWatchRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteWatchRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl WatchRepository for SqliteWatchRepository<'_> {
    async fn watch(&self, user_id: UserId, page_id: PageId) -> Result<bool, StorageError> {
        let user_bytes = uuid_bytes(user_id.into_uuid());
        let page_bytes = uuid_bytes(page_id.into_uuid());
        let created_at = format_ts(OffsetDateTime::now_utc())?;
        // `INSERT OR IGNORE` keeps the original `created_at` if the row was
        // already there — re-watching is a no-op, not a "reset the date".
        // `rows_affected()` reports 0 for the ignored case and 1 when a new
        // row was inserted; we surface that to the caller so duplicate POSTs
        // don't generate spurious audit-log entries.
        let result = sqlx::query(
            "INSERT OR IGNORE INTO watch (user_id, page_id, created_at)
             VALUES (?1, ?2, ?3)",
        )
        .bind(user_bytes.as_slice())
        .bind(page_bytes.as_slice())
        .bind(&created_at)
        .execute(self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn unwatch(&self, user_id: UserId, page_id: PageId) -> Result<bool, StorageError> {
        let user_bytes = uuid_bytes(user_id.into_uuid());
        let page_bytes = uuid_bytes(page_id.into_uuid());
        let result = sqlx::query("DELETE FROM watch WHERE user_id = ?1 AND page_id = ?2")
            .bind(user_bytes.as_slice())
            .bind(page_bytes.as_slice())
            .execute(self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    async fn is_watched(&self, user_id: UserId, page_id: PageId) -> Result<bool, StorageError> {
        let user_bytes = uuid_bytes(user_id.into_uuid());
        let page_bytes = uuid_bytes(page_id.into_uuid());
        let row: Option<(i64,)> =
            sqlx::query_as("SELECT 1 FROM watch WHERE user_id = ?1 AND page_id = ?2 LIMIT 1")
                .bind(user_bytes.as_slice())
                .bind(page_bytes.as_slice())
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
        let user_bytes = uuid_bytes(user_id.into_uuid());

        let rows: Vec<WatchJoinRow> = sqlx::query_as(
            "SELECT pages.id, pages.namespace_id, namespaces.slug,
                    pages.slug, pages.title, pages.protection_level,
                    watch.created_at, pages.updated_at
             FROM watch
             JOIN pages      ON pages.id          = watch.page_id
             JOIN namespaces ON namespaces.id     = pages.namespace_id
             WHERE watch.user_id = ?1
             ORDER BY watch.created_at DESC, watch.page_id DESC
             LIMIT ?2",
        )
        .bind(user_bytes.as_slice())
        .bind(take)
        .fetch_all(self.pool)
        .await?;

        rows.into_iter()
            .map(
                |(id, ns_id, ns_slug, page_slug, title, prot, watched_at, updated_at)| {
                    Ok(WatchedPage {
                        page_id: PageId::from_uuid(decode_uuid(&id)?),
                        namespace_id: NamespaceId::from_uuid(decode_uuid(&ns_id)?),
                        namespace_slug: ns_slug,
                        page_slug,
                        page_title: title,
                        protection_level: parse_protection_level(&prot)?,
                        watched_at: parse_ts(&watched_at)?,
                        updated_at: parse_ts(&updated_at)?,
                    })
                },
            )
            .collect()
    }
}
