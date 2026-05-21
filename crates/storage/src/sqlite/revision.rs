//! SQLite [`RevisionRepository`](crate::repo::RevisionRepository) impl.

use sqlx::SqlitePool;
use thewiki_core::{PageId, Revision, RevisionId};

use crate::error::StorageError;
use crate::repo::{Cursor, PageSlice, RevisionRepository, clamp_limit};
use crate::sqlite::codec::{format_ts, hex_decode_id, hex_encode, revision_from_row, uuid_bytes};

type RevisionRow = (
    Vec<u8>,         // id
    Vec<u8>,         // page_id
    Option<Vec<u8>>, // parent_id
    Vec<u8>,         // author_id
    String,          // body
    Option<String>,  // edit_summary
    String,          // created_at
);

fn row_to_revision(row: RevisionRow) -> Result<Revision, StorageError> {
    let (id, page_id, parent_id, author_id, body, edit_summary, created_at) = row;
    revision_from_row(
        id,
        page_id,
        parent_id,
        author_id,
        body,
        edit_summary,
        created_at,
    )
}

/// SQLite-backed revision repository.
pub struct SqliteRevisionRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteRevisionRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl RevisionRepository for SqliteRevisionRepository<'_> {
    async fn create(&self, revision: &Revision) -> Result<(), StorageError> {
        let id = uuid_bytes(revision.id.into_uuid());
        let page_id = uuid_bytes(revision.page_id.into_uuid());
        let parent_id = revision.parent_id.map(|r| uuid_bytes(r.into_uuid()));
        let author_id = uuid_bytes(revision.author_id.into_uuid());
        let created_at = format_ts(revision.created_at)?;

        sqlx::query(
            "INSERT INTO revisions
                (id, page_id, parent_id, author_id, body, edit_summary, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(id.as_slice())
        .bind(page_id.as_slice())
        .bind(parent_id.as_ref().map(|b| b.as_slice()))
        .bind(author_id.as_slice())
        .bind(&revision.body)
        .bind(revision.edit_summary.as_deref())
        .bind(&created_at)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    async fn get_by_id(&self, id: RevisionId) -> Result<Revision, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let row: Option<RevisionRow> = sqlx::query_as(
            "SELECT id, page_id, parent_id, author_id, body, edit_summary, created_at
             FROM revisions WHERE id = ?1",
        )
        .bind(id_bytes.as_slice())
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some(row) => row_to_revision(row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn list_for_page(
        &self,
        page_id: PageId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<PageSlice<Revision>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit) + 1;
        let page_bytes = uuid_bytes(page_id.into_uuid());

        // Listing is newest-first. Cursor encodes the last
        // `(created_at, id)` we returned and we fetch rows strictly older.
        let rows: Vec<RevisionRow> = if let Some(cursor) = cursor {
            let (ts, id_hex) = decode_cursor(&cursor)?;
            let id_bytes = hex_decode_id(&id_hex)?;
            sqlx::query_as(
                "SELECT id, page_id, parent_id, author_id, body, edit_summary, created_at
                 FROM revisions
                 WHERE page_id = ?1
                   AND (created_at, id) < (?2, ?3)
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?4",
            )
            .bind(page_bytes.as_slice())
            .bind(ts)
            .bind(id_bytes.as_slice())
            .bind(take)
            .fetch_all(self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, page_id, parent_id, author_id, body, edit_summary, created_at
                 FROM revisions
                 WHERE page_id = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?2",
            )
            .bind(page_bytes.as_slice())
            .bind(take)
            .fetch_all(self.pool)
            .await?
        };

        finalise_page(rows, limit)
    }

    async fn head_of(&self, page_id: PageId) -> Result<Revision, StorageError> {
        let page_bytes = uuid_bytes(page_id.into_uuid());
        let row: Option<RevisionRow> = sqlx::query_as(
            "SELECT id, page_id, parent_id, author_id, body, edit_summary, created_at
             FROM revisions
             WHERE page_id = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
        )
        .bind(page_bytes.as_slice())
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some(row) => row_to_revision(row),
            None => Err(StorageError::NotFound),
        }
    }
}

fn encode_cursor(created_at: &str, id: &[u8]) -> Cursor {
    Cursor(format!("{}|{}", created_at, hex_encode(id)))
}

fn decode_cursor(c: &Cursor) -> Result<(String, String), StorageError> {
    let (ts, id_hex) = c.0.split_once('|').ok_or_else(|| {
        StorageError::invalid_input("revision cursor must be `<timestamp>|<hex-id>`")
    })?;
    Ok((ts.to_string(), id_hex.to_string()))
}

fn finalise_page(
    mut rows: Vec<RevisionRow>,
    limit: u32,
) -> Result<PageSlice<Revision>, StorageError> {
    let limit_usize = limit as usize;
    let has_more = rows.len() > limit_usize;
    if has_more {
        rows.truncate(limit_usize);
    }
    let next = if has_more {
        rows.last().map(|last| encode_cursor(&last.6, &last.0))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(row_to_revision)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PageSlice { items, next })
}
