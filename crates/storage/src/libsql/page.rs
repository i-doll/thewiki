//! libsql [`PageRepository`](crate::repo::PageRepository) impl.

use libsql::{Connection, Value, params};
use thewiki_core::{NamespaceId, Page, PageId};

use crate::error::StorageError;
use crate::libsql::codec::{
    format_ts, hex_decode_id, hex_encode, into_db, map_unique_violation, opt_blob,
    page_from_libsql_row, uuid_bytes,
};
use crate::repo::{Cursor, PageRepository, PageSlice, clamp_limit};

/// libsql-backed page repository.
pub struct LibsqlPageRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LibsqlPageRepository<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl PageRepository for LibsqlPageRepository<'_> {
    async fn create(&self, page: &Page) -> Result<(), StorageError> {
        let id = uuid_bytes(page.id.into_uuid());
        let namespace_id = uuid_bytes(page.namespace_id.into_uuid());
        let current_rev = page.current_revision_id.map(|r| uuid_bytes(r.into_uuid()));
        let created_at = format_ts(page.created_at)?;
        let updated_at = format_ts(page.updated_at)?;

        let result = self
            .conn
            .execute(
                "INSERT INTO pages
                    (id, namespace_id, slug, title, current_revision_id,
                     content_format, protection_level, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    Value::Blob(id.to_vec()),
                    Value::Blob(namespace_id.to_vec()),
                    page.slug.clone(),
                    page.title.clone(),
                    opt_blob(current_rev.as_ref().map(|b| b.as_slice())),
                    page.content_format.as_str().to_owned(),
                    page.protection_level.as_str().to_owned(),
                    created_at,
                    updated_at,
                ],
            )
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
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, namespace_id, slug, title, current_revision_id,
                            content_format, protection_level, created_at, updated_at
                     FROM pages WHERE id = ?1",
                    params![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        match row {
            Some(row) => page_from_libsql_row(&row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_namespace_and_slug(
        &self,
        namespace_id: NamespaceId,
        slug: &str,
    ) -> Result<Page, StorageError> {
        let ns = uuid_bytes(namespace_id.into_uuid());
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, namespace_id, slug, title, current_revision_id,
                            content_format, protection_level, created_at, updated_at
                     FROM pages WHERE namespace_id = ?1 AND slug = ?2",
                    params![Value::Blob(ns.to_vec()), slug.to_owned()],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        match row {
            Some(row) => page_from_libsql_row(&row),
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
        let take = i64::from(limit) + 1;

        let mut rows_iter = if let Some(cursor) = cursor {
            let (ts, id_hex) = decode_cursor(&cursor)?;
            let id_bytes = hex_decode_id(&id_hex)?;
            into_db(
                self.conn
                    .query(
                        "SELECT id, namespace_id, slug, title, current_revision_id,
                                content_format, protection_level, created_at, updated_at
                         FROM pages
                         WHERE namespace_id = ?1
                           AND (created_at, id) > (?2, ?3)
                         ORDER BY created_at ASC, id ASC
                         LIMIT ?4",
                        params![
                            Value::Blob(ns.to_vec()),
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
                        "SELECT id, namespace_id, slug, title, current_revision_id,
                                content_format, protection_level, created_at, updated_at
                         FROM pages
                         WHERE namespace_id = ?1
                         ORDER BY created_at ASC, id ASC
                         LIMIT ?2",
                        params![Value::Blob(ns.to_vec()), take],
                    )
                    .await,
            )?
        };

        // libsql streams rows; collect them into our paging structure.
        let mut collected: Vec<(String, [u8; 16], Page)> = Vec::with_capacity(limit as usize + 1);
        while let Some(row) = into_db(rows_iter.next().await)? {
            let id_blob: Vec<u8> = into_db(row.get::<Vec<u8>>(0))?;
            let id_arr: [u8; 16] = id_blob
                .as_slice()
                .try_into()
                .map_err(|_| StorageError::invalid_input("page id column wrong size"))?;
            let created_at: String = into_db(row.get::<String>(7))?;
            let page = page_from_libsql_row(&row)?;
            collected.push((created_at, id_arr, page));
        }
        finalise(collected, limit)
    }

    async fn update(&self, page: &Page) -> Result<(), StorageError> {
        let id = uuid_bytes(page.id.into_uuid());
        let current_rev = page.current_revision_id.map(|r| uuid_bytes(r.into_uuid()));
        let updated_at = format_ts(page.updated_at)?;

        let result = self
            .conn
            .execute(
                "UPDATE pages
                 SET slug = ?1,
                     title = ?2,
                     current_revision_id = ?3,
                     content_format = ?4,
                     protection_level = ?5,
                     updated_at = ?6
                 WHERE id = ?7",
                params![
                    page.slug.clone(),
                    page.title.clone(),
                    opt_blob(current_rev.as_ref().map(|b| b.as_slice())),
                    page.content_format.as_str().to_owned(),
                    page.protection_level.as_str().to_owned(),
                    updated_at,
                    Value::Blob(id.to_vec()),
                ],
            )
            .await;

        match result {
            Ok(rows_affected) => {
                if rows_affected == 0 {
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
        let rows_affected = into_db(
            self.conn
                .execute(
                    "DELETE FROM pages WHERE id = ?1",
                    params![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        if rows_affected == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }
}

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

fn finalise(
    mut rows: Vec<(String, [u8; 16], Page)>,
    limit: u32,
) -> Result<PageSlice<Page>, StorageError> {
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
    let items = rows.into_iter().map(|(_, _, p)| p).collect();
    Ok(PageSlice { items, next })
}
