//! SQLite [`PageRepository`](crate::repo::PageRepository) impl.

use sqlx::SqlitePool;
use thewiki_core::{NamespaceId, Page, PageId};

use crate::error::StorageError;
use crate::repo::{Cursor, PageRepository, PageSlice, clamp_limit};
use crate::sqlite::codec::{
    format_ts, hex_decode_id, hex_encode, map_unique_violation, page_from_row, uuid_bytes,
};

/// Shape of a `pages` row coming back from the driver.
type PageRow = (
    Vec<u8>,         // id
    Vec<u8>,         // namespace_id
    String,          // slug
    String,          // title
    Option<Vec<u8>>, // current_revision_id
    String,          // content_format
    String,          // protection_level
    String,          // created_at
    String,          // updated_at
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

/// SQLite-backed page repository.
pub struct SqlitePageRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqlitePageRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl PageRepository for SqlitePageRepository<'_> {
    async fn create(&self, page: &Page) -> Result<(), StorageError> {
        let id = uuid_bytes(page.id.into_uuid());
        let namespace_id = uuid_bytes(page.namespace_id.into_uuid());
        let current_rev = page.current_revision_id.map(|r| uuid_bytes(r.into_uuid()));
        let created_at = format_ts(page.created_at)?;
        let updated_at = format_ts(page.updated_at)?;

        let result = sqlx::query(
            "INSERT INTO pages
                (id, namespace_id, slug, title, current_revision_id,
                 content_format, protection_level, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(id.as_slice())
        .bind(namespace_id.as_slice())
        .bind(&page.slug)
        .bind(&page.title)
        .bind(current_rev.as_ref().map(|b| b.as_slice()))
        .bind(page.content_format.as_str())
        .bind(page.protection_level.as_str())
        .bind(&created_at)
        .bind(&updated_at)
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
        let id_bytes = uuid_bytes(id.into_uuid());
        let row: Option<PageRow> = sqlx::query_as(
            "SELECT id, namespace_id, slug, title, current_revision_id,
                    content_format, protection_level, created_at, updated_at
             FROM pages WHERE id = ?1",
        )
        .bind(id_bytes.as_slice())
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
        let ns = uuid_bytes(namespace_id.into_uuid());
        let row: Option<PageRow> = sqlx::query_as(
            "SELECT id, namespace_id, slug, title, current_revision_id,
                    content_format, protection_level, created_at, updated_at
             FROM pages WHERE namespace_id = ?1 AND slug = ?2",
        )
        .bind(ns.as_slice())
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
        let ns = uuid_bytes(namespace_id.into_uuid());

        // Cursor encodes the last `(created_at, id)` we returned. We fetch
        // `limit + 1` rows so the presence of an N+1th tells us another page
        // is available; we then drop it and emit it as the next cursor.
        let take = i64::from(limit) + 1;

        let rows: Vec<PageRow> = if let Some(cursor) = cursor {
            let (ts, id_hex) = decode_cursor(&cursor)?;
            let id_bytes = hex_decode_id(&id_hex)?;
            sqlx::query_as(
                "SELECT id, namespace_id, slug, title, current_revision_id,
                        content_format, protection_level, created_at, updated_at
                 FROM pages
                 WHERE namespace_id = ?1
                   AND (created_at, id) > (?2, ?3)
                 ORDER BY created_at ASC, id ASC
                 LIMIT ?4",
            )
            .bind(ns.as_slice())
            .bind(ts)
            .bind(id_bytes.as_slice())
            .bind(take)
            .fetch_all(self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, namespace_id, slug, title, current_revision_id,
                        content_format, protection_level, created_at, updated_at
                 FROM pages
                 WHERE namespace_id = ?1
                 ORDER BY created_at ASC, id ASC
                 LIMIT ?2",
            )
            .bind(ns.as_slice())
            .bind(take)
            .fetch_all(self.pool)
            .await?
        };

        finalise_page(rows, limit)
    }

    async fn update(&self, page: &Page) -> Result<(), StorageError> {
        let id = uuid_bytes(page.id.into_uuid());
        let current_rev = page.current_revision_id.map(|r| uuid_bytes(r.into_uuid()));
        let updated_at = format_ts(page.updated_at)?;

        let result = sqlx::query(
            "UPDATE pages
             SET slug = ?1,
                 title = ?2,
                 current_revision_id = ?3,
                 content_format = ?4,
                 protection_level = ?5,
                 updated_at = ?6
             WHERE id = ?7",
        )
        .bind(&page.slug)
        .bind(&page.title)
        .bind(current_rev.as_ref().map(|b| b.as_slice()))
        .bind(page.content_format.as_str())
        .bind(page.protection_level.as_str())
        .bind(&updated_at)
        .bind(id.as_slice())
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
        let id_bytes = uuid_bytes(id.into_uuid());
        let result = sqlx::query("DELETE FROM pages WHERE id = ?1")
            .bind(id_bytes.as_slice())
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
fn encode_cursor(created_at: &str, id: &[u8]) -> Cursor {
    Cursor(format!("{}|{}", created_at, hex_encode(id)))
}

fn decode_cursor(c: &Cursor) -> Result<(String, String), StorageError> {
    let (ts, id_hex) = c
        .0
        .split_once('|')
        .ok_or_else(|| StorageError::invalid_input("page cursor must be `<timestamp>|<hex-id>`"))?;
    Ok((ts.to_string(), id_hex.to_string()))
}

fn finalise_page(mut rows: Vec<PageRow>, limit: u32) -> Result<PageSlice<Page>, StorageError> {
    let limit_usize = limit as usize;
    let has_more = rows.len() > limit_usize;
    if has_more {
        // Drop the (limit+1)th probe row; it told us "more exists" but the
        // caller should not see it.
        rows.truncate(limit_usize);
    }
    // The cursor anchors at the LAST returned row so the next page resumes
    // strictly after it.
    let next = if has_more {
        rows.last().map(|last| encode_cursor(&last.7, &last.0))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(row_to_page)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PageSlice { items, next })
}
