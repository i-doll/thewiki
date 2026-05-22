//! Postgres [`RevisionRepository`](crate::repo::RevisionRepository) impl.

use sqlx::PgPool;
use thewiki_core::{PageId, Revision, RevisionId};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::StorageError;
use crate::postgres::codec::{format_cursor_ts, parse_cursor_ts, revision_from_row};
use crate::repo::{Cursor, PageSlice, RevisionRepository, clamp_limit};

type RevisionRow = (
    Uuid,           // id
    Uuid,           // page_id
    Option<Uuid>,   // parent_id
    Uuid,           // author_id
    String,         // body
    Option<String>, // edit_summary
    OffsetDateTime, // created_at
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

/// Postgres-backed revision repository.
pub struct PostgresRevisionRepository<'a> {
    pool: &'a PgPool,
}

impl<'a> PostgresRevisionRepository<'a> {
    pub(super) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }
}

impl RevisionRepository for PostgresRevisionRepository<'_> {
    async fn create(&self, revision: &Revision) -> Result<(), StorageError> {
        let parent_id = revision.parent_id.map(|r| r.into_uuid());
        sqlx::query(
            "INSERT INTO revisions
                (id, page_id, parent_id, author_id, body, edit_summary, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(revision.id.into_uuid())
        .bind(revision.page_id.into_uuid())
        .bind(parent_id)
        .bind(revision.author_id.into_uuid())
        .bind(&revision.body)
        .bind(revision.edit_summary.as_deref())
        .bind(revision.created_at)
        .execute(self.pool)
        .await?;
        Ok(())
    }

    async fn get_by_id(&self, id: RevisionId) -> Result<Revision, StorageError> {
        let row: Option<RevisionRow> = sqlx::query_as(
            "SELECT id, page_id, parent_id, author_id, body, edit_summary, created_at
             FROM revisions WHERE id = $1",
        )
        .bind(id.into_uuid())
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

        // Listing is newest-first. Cursor encodes the last
        // `(created_at, id)` we returned and we fetch rows strictly older.
        let rows: Vec<RevisionRow> = if let Some(cursor) = cursor {
            let (ts, id) = decode_cursor(&cursor)?;
            sqlx::query_as(
                "SELECT id, page_id, parent_id, author_id, body, edit_summary, created_at
                 FROM revisions
                 WHERE page_id = $1
                   AND (created_at, id) < ($2, $3)
                 ORDER BY created_at DESC, id DESC
                 LIMIT $4",
            )
            .bind(page_id.into_uuid())
            .bind(ts)
            .bind(id)
            .bind(take)
            .fetch_all(self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, page_id, parent_id, author_id, body, edit_summary, created_at
                 FROM revisions
                 WHERE page_id = $1
                 ORDER BY created_at DESC, id DESC
                 LIMIT $2",
            )
            .bind(page_id.into_uuid())
            .bind(take)
            .fetch_all(self.pool)
            .await?
        };

        finalise_page(rows, limit)
    }

    async fn head_of(&self, page_id: PageId) -> Result<Revision, StorageError> {
        let row: Option<RevisionRow> = sqlx::query_as(
            "SELECT id, page_id, parent_id, author_id, body, edit_summary, created_at
             FROM revisions
             WHERE page_id = $1
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
        )
        .bind(page_id.into_uuid())
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some(row) => row_to_revision(row),
            None => Err(StorageError::NotFound),
        }
    }
}

fn encode_cursor(created_at: OffsetDateTime, id: Uuid) -> Result<Cursor, StorageError> {
    Ok(Cursor(format!("{}|{}", format_cursor_ts(created_at)?, id)))
}

fn decode_cursor(c: &Cursor) -> Result<(OffsetDateTime, Uuid), StorageError> {
    let (ts, id_str) = c.0.split_once('|').ok_or_else(|| {
        StorageError::invalid_input("revision cursor must be `<timestamp>|<uuid>`")
    })?;
    let ts = parse_cursor_ts(ts)?;
    let id = Uuid::parse_str(id_str)
        .map_err(|err| StorageError::invalid_input(format!("revision cursor uuid: {err}")))?;
    Ok((ts, id))
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
        match rows.last() {
            Some(last) => Some(encode_cursor(last.6, last.0)?),
            None => None,
        }
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(row_to_revision)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PageSlice { items, next })
}
