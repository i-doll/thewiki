//! Postgres [`PageRepository`](crate::repo::PageRepository) impl.

use sqlx::PgPool;
use thewiki_core::{NamespaceId, Page, PageId};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::StorageError;
use crate::postgres::codec::{
    format_cursor_ts, map_unique_violation, page_from_row, parse_cursor_ts,
};
use crate::repo::{Cursor, PageRepository, PageSlice, clamp_limit};

/// Shape of a `pages` row coming back from the driver.
type PageRow = (
    Uuid,           // id
    Uuid,           // namespace_id
    String,         // slug
    String,         // title
    Option<Uuid>,   // current_revision_id
    String,         // content_format
    String,         // protection_level
    OffsetDateTime, // created_at
    OffsetDateTime, // updated_at
);

fn row_to_page(row: PageRow) -> Result<Page, StorageError> {
    let (
        id,
        namespace_id,
        slug,
        title,
        current_revision_id,
        content_format,
        protection_level,
        created_at,
        updated_at,
    ) = row;
    page_from_row(
        id,
        namespace_id,
        slug,
        title,
        current_revision_id,
        content_format,
        protection_level,
        created_at,
        updated_at,
    )
}

/// Postgres-backed page repository.
pub struct PostgresPageRepository<'a> {
    pool: &'a PgPool,
}

impl<'a> PostgresPageRepository<'a> {
    pub(super) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }
}

impl PageRepository for PostgresPageRepository<'_> {
    async fn create(&self, page: &Page) -> Result<(), StorageError> {
        let id = page.id.into_uuid();
        let namespace_id = page.namespace_id.into_uuid();
        let current_rev = page.current_revision_id.map(|r| r.into_uuid());

        let result = sqlx::query(
            "INSERT INTO pages
                (id, namespace_id, slug, title, current_revision_id,
                 content_format, protection_level, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(namespace_id)
        .bind(&page.slug)
        .bind(&page.title)
        .bind(current_rev)
        .bind(page.content_format.as_str())
        .bind(page.protection_level.as_str())
        .bind(page.created_at)
        .bind(page.updated_at)
        .execute(self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(
                err,
                "page slug already exists in namespace",
            )),
        }
    }

    async fn get_by_id(&self, id: PageId) -> Result<Page, StorageError> {
        let row: Option<PageRow> = sqlx::query_as(
            "SELECT id, namespace_id, slug, title, current_revision_id,
                    content_format, protection_level, created_at, updated_at
             FROM pages WHERE id = $1",
        )
        .bind(id.into_uuid())
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some(row) => row_to_page(row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_namespace_and_slug(
        &self,
        namespace_id: NamespaceId,
        slug: &str,
    ) -> Result<Page, StorageError> {
        let row: Option<PageRow> = sqlx::query_as(
            "SELECT id, namespace_id, slug, title, current_revision_id,
                    content_format, protection_level, created_at, updated_at
             FROM pages WHERE namespace_id = $1 AND slug = $2",
        )
        .bind(namespace_id.into_uuid())
        .bind(slug)
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some(row) => row_to_page(row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn list_in_namespace(
        &self,
        namespace_id: NamespaceId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<PageSlice<Page>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit) + 1;

        // Cursor encodes the last `(created_at, id)` we returned. We fetch
        // `limit + 1` rows so the presence of an N+1th tells us another page
        // is available; we then drop it and emit it as the next cursor.
        //
        // Postgres' row-value comparison `(created_at, id) > ($2, $3)` walks
        // the two-column index on `(created_at, id)` directly, so the cursor
        // resumes the scan without rescanning earlier rows.
        let rows: Vec<PageRow> = if let Some(cursor) = cursor {
            let (ts, id) = decode_cursor(&cursor)?;
            sqlx::query_as(
                "SELECT id, namespace_id, slug, title, current_revision_id,
                        content_format, protection_level, created_at, updated_at
                 FROM pages
                 WHERE namespace_id = $1
                   AND (created_at, id) > ($2, $3)
                 ORDER BY created_at ASC, id ASC
                 LIMIT $4",
            )
            .bind(namespace_id.into_uuid())
            .bind(ts)
            .bind(id)
            .bind(take)
            .fetch_all(self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, namespace_id, slug, title, current_revision_id,
                        content_format, protection_level, created_at, updated_at
                 FROM pages
                 WHERE namespace_id = $1
                 ORDER BY created_at ASC, id ASC
                 LIMIT $2",
            )
            .bind(namespace_id.into_uuid())
            .bind(take)
            .fetch_all(self.pool)
            .await?
        };

        finalise_page(rows, limit)
    }

    async fn update(&self, page: &Page) -> Result<(), StorageError> {
        let current_rev = page.current_revision_id.map(|r| r.into_uuid());

        let result = sqlx::query(
            "UPDATE pages
             SET slug = $1,
                 title = $2,
                 current_revision_id = $3,
                 content_format = $4,
                 protection_level = $5,
                 updated_at = $6
             WHERE id = $7",
        )
        .bind(&page.slug)
        .bind(&page.title)
        .bind(current_rev)
        .bind(page.content_format.as_str())
        .bind(page.protection_level.as_str())
        .bind(page.updated_at)
        .bind(page.id.into_uuid())
        .execute(self.pool)
        .await;

        match result {
            Ok(out) => {
                if out.rows_affected() == 0 {
                    Err(StorageError::NotFound)
                } else {
                    Ok(())
                }
            }
            Err(err) => Err(map_unique_violation(
                err,
                "page slug already exists in namespace",
            )),
        }
    }

    async fn delete(&self, id: PageId) -> Result<(), StorageError> {
        let result = sqlx::query("DELETE FROM pages WHERE id = $1")
            .bind(id.into_uuid())
            .execute(self.pool)
            .await?;
        if result.rows_affected() == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }
}

/// Encode a `(created_at, id)` pair as the opaque cursor string we hand
/// back to callers.
fn encode_cursor(created_at: OffsetDateTime, id: Uuid) -> Result<Cursor, StorageError> {
    Ok(Cursor(format!("{}|{}", format_cursor_ts(created_at)?, id)))
}

fn decode_cursor(c: &Cursor) -> Result<(OffsetDateTime, Uuid), StorageError> {
    let (ts, id_str) = c
        .0
        .split_once('|')
        .ok_or_else(|| StorageError::invalid_input("page cursor must be `<timestamp>|<uuid>`"))?;
    let ts = parse_cursor_ts(ts)?;
    let id = Uuid::parse_str(id_str)
        .map_err(|err| StorageError::invalid_input(format!("page cursor uuid: {err}")))?;
    Ok((ts, id))
}

fn finalise_page(mut rows: Vec<PageRow>, limit: u32) -> Result<PageSlice<Page>, StorageError> {
    let limit_usize = limit as usize;
    let has_more = rows.len() > limit_usize;
    if has_more {
        rows.truncate(limit_usize);
    }
    let next = if has_more {
        match rows.last() {
            Some(last) => Some(encode_cursor(last.7, last.0)?),
            None => None,
        }
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(row_to_page)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PageSlice { items, next })
}
