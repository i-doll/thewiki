//! libsql [`RevisionRepository`](crate::repo::RevisionRepository) impl.

use libsql::{Connection, Value, params};
use thewiki_core::{PageId, Revision, RevisionId};

use crate::error::StorageError;
use crate::libsql::codec::{
    format_ts, hex_decode_id, hex_encode, into_db, opt_blob, revision_from_libsql_row, uuid_bytes,
};
use crate::repo::{Cursor, PageSlice, RevisionRepository, clamp_limit};

/// libsql-backed revision repository.
pub struct LibsqlRevisionRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LibsqlRevisionRepository<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl RevisionRepository for LibsqlRevisionRepository<'_> {
    async fn create(&self, revision: &Revision) -> Result<(), StorageError> {
        let id = uuid_bytes(revision.id.into_uuid());
        let page_id = uuid_bytes(revision.page_id.into_uuid());
        let parent_id = revision.parent_id.map(|r| uuid_bytes(r.into_uuid()));
        let author_id = uuid_bytes(revision.author_id.into_uuid());
        let created_at = format_ts(revision.created_at)?;

        into_db(
            self.conn
                .execute(
                    "INSERT INTO revisions
                        (id, page_id, parent_id, author_id, body, edit_summary, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        Value::Blob(id.to_vec()),
                        Value::Blob(page_id.to_vec()),
                        opt_blob(parent_id.as_ref().map(|b| b.as_slice())),
                        Value::Blob(author_id.to_vec()),
                        revision.body.clone(),
                        match revision.edit_summary.as_deref() {
                            Some(s) => Value::Text(s.to_owned()),
                            None => Value::Null,
                        },
                        created_at,
                    ],
                )
                .await,
        )?;
        Ok(())
    }

    async fn get_by_id(&self, id: RevisionId) -> Result<Revision, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, page_id, parent_id, author_id, body, edit_summary, created_at
                     FROM revisions WHERE id = ?1",
                    params![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        match row {
            Some(row) => revision_from_libsql_row(&row),
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

        let mut rows_iter = if let Some(cursor) = cursor {
            let (ts, id_hex) = decode_cursor(&cursor)?;
            let id_bytes = hex_decode_id(&id_hex)?;
            into_db(
                self.conn
                    .query(
                        "SELECT id, page_id, parent_id, author_id, body, edit_summary, created_at
                         FROM revisions
                         WHERE page_id = ?1
                           AND (created_at, id) < (?2, ?3)
                         ORDER BY created_at DESC, id DESC
                         LIMIT ?4",
                        params![
                            Value::Blob(page_bytes.to_vec()),
                            ts,
                            Value::Blob(id_bytes.to_vec()),
                            take
                        ],
                    )
                    .await,
            )?
        } else {
            into_db(
                self.conn
                    .query(
                        "SELECT id, page_id, parent_id, author_id, body, edit_summary, created_at
                         FROM revisions
                         WHERE page_id = ?1
                         ORDER BY created_at DESC, id DESC
                         LIMIT ?2",
                        params![Value::Blob(page_bytes.to_vec()), take],
                    )
                    .await,
            )?
        };

        let mut collected: Vec<(String, [u8; 16], Revision)> =
            Vec::with_capacity(limit as usize + 1);
        while let Some(row) = into_db(rows_iter.next().await)? {
            let id_blob: Vec<u8> = into_db(row.get::<Vec<u8>>(0))?;
            let id_arr: [u8; 16] = id_blob
                .as_slice()
                .try_into()
                .map_err(|_| StorageError::invalid_input("revision id column wrong size"))?;
            let created_at: String = into_db(row.get::<String>(6))?;
            let revision = revision_from_libsql_row(&row)?;
            collected.push((created_at, id_arr, revision));
        }
        finalise(collected, limit)
    }

    async fn head_of(&self, page_id: PageId) -> Result<Revision, StorageError> {
        let page_bytes = uuid_bytes(page_id.into_uuid());
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, page_id, parent_id, author_id, body, edit_summary, created_at
                     FROM revisions
                     WHERE page_id = ?1
                     ORDER BY created_at DESC, id DESC
                     LIMIT 1",
                    params![Value::Blob(page_bytes.to_vec())],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        match row {
            Some(row) => revision_from_libsql_row(&row),
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

fn finalise(
    mut rows: Vec<(String, [u8; 16], Revision)>,
    limit: u32,
) -> Result<PageSlice<Revision>, StorageError> {
    let limit_usize = limit as usize;
    let has_more = rows.len() > limit_usize;
    if has_more {
        rows.truncate(limit_usize);
    }
    let next = if has_more {
        rows.last().map(|(ts, id, _)| encode_cursor(ts, id))
    } else {
        None
    };
    let items = rows.into_iter().map(|(_, _, r)| r).collect();
    Ok(PageSlice { items, next })
}
