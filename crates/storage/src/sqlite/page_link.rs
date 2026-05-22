//! SQLite [`PageLinkRepository`](crate::repo::PageLinkRepository) impl.
//!
//! Backs the outbound-wikilink graph that powers the backlinks API (#30).
//! The table is populated by the API layer on page create / update; this
//! module only persists / queries it.

use sqlx::SqlitePool;
use thewiki_core::{NamespaceId, PageId};

use crate::error::StorageError;
use crate::repo::{BacklinkRow, Cursor, PageLink, PageLinkRepository, PageSlice, clamp_limit};
use crate::sqlite::codec::{hex_decode_id, hex_encode, uuid_bytes};

/// Shape of a backlink row returned by the JOIN.
///
/// Pulled out as a `type` alias to keep clippy's complexity lint happy and
/// to keep the two query branches in sync.
type BacklinkJoinRow = (
    Vec<u8>, // pages.id
    Vec<u8>, // pages.namespace_id
    String,  // namespaces.slug
    String,  // pages.slug
    String,  // pages.title
);

/// SQLite-backed page-link repository.
pub struct SqlitePageLinkRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqlitePageLinkRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl PageLinkRepository for SqlitePageLinkRepository<'_> {
    async fn replace_for_source(
        &self,
        source_page_id: PageId,
        links: &[PageLink],
    ) -> Result<(), StorageError> {
        // Atomic swap: drop the existing rows for this source, then insert
        // the new ones. Wrapping both in a single transaction keeps a reader
        // from observing a "links wiped, nothing yet inserted" intermediate
        // state.
        let mut tx = self.pool.begin().await?;
        let source = uuid_bytes(source_page_id.into_uuid());
        sqlx::query("DELETE FROM page_links WHERE source_page_id = ?1")
            .bind(source.as_slice())
            .execute(&mut *tx)
            .await?;

        // Insert with OR IGNORE so a caller that accidentally passes the
        // same `(target_ns, target_slug)` twice doesn't bomb the whole
        // commit. The renderer's extract_links can yield duplicates when a
        // page references the same target multiple times.
        for link in links {
            if link.source_page_id != source_page_id {
                return Err(StorageError::invalid_input(
                    "PageLink.source_page_id must match the replace_for_source argument",
                ));
            }
            sqlx::query(
                "INSERT OR IGNORE INTO page_links
                    (source_page_id, target_namespace_slug, target_page_slug)
                 VALUES (?1, ?2, ?3)",
            )
            .bind(source.as_slice())
            .bind(&link.target_namespace_slug)
            .bind(&link.target_page_slug)
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    async fn delete_for_source(&self, source_page_id: PageId) -> Result<(), StorageError> {
        let source = uuid_bytes(source_page_id.into_uuid());
        sqlx::query("DELETE FROM page_links WHERE source_page_id = ?1")
            .bind(source.as_slice())
            .execute(self.pool)
            .await?;
        Ok(())
    }

    async fn list_backlinks_to(
        &self,
        target_namespace_slug: &str,
        target_page_slug: &str,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<PageSlice<BacklinkRow>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit) + 1;

        // Same `limit + 1` probe trick as `list_in_namespace`: an N+1th row
        // signals "another page exists" and is dropped before returning.
        // Cursor encodes the last `source_page_id` returned.
        let rows: Vec<BacklinkJoinRow> = if let Some(cursor) = cursor {
            let id_bytes = hex_decode_id(cursor.as_str())?;
            sqlx::query_as(
                "SELECT pages.id, pages.namespace_id, namespaces.slug,
                        pages.slug, pages.title
                 FROM page_links
                 JOIN pages ON pages.id = page_links.source_page_id
                 JOIN namespaces ON namespaces.id = pages.namespace_id
                 WHERE page_links.target_namespace_slug = ?1
                   AND page_links.target_page_slug = ?2
                   AND pages.id > ?3
                 ORDER BY pages.id ASC
                 LIMIT ?4",
            )
            .bind(target_namespace_slug)
            .bind(target_page_slug)
            .bind(id_bytes.as_slice())
            .bind(take)
            .fetch_all(self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT pages.id, pages.namespace_id, namespaces.slug,
                        pages.slug, pages.title
                 FROM page_links
                 JOIN pages ON pages.id = page_links.source_page_id
                 JOIN namespaces ON namespaces.id = pages.namespace_id
                 WHERE page_links.target_namespace_slug = ?1
                   AND page_links.target_page_slug = ?2
                 ORDER BY pages.id ASC
                 LIMIT ?3",
            )
            .bind(target_namespace_slug)
            .bind(target_page_slug)
            .bind(take)
            .fetch_all(self.pool)
            .await?
        };

        finalise(rows, limit)
    }
}

fn finalise(
    mut rows: Vec<BacklinkJoinRow>,
    limit: u32,
) -> Result<PageSlice<BacklinkRow>, StorageError> {
    let limit_usize = limit as usize;
    let has_more = rows.len() > limit_usize;
    if has_more {
        rows.truncate(limit_usize);
    }
    let next = if has_more {
        rows.last().map(|last| Cursor(hex_encode(&last.0)))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(|(id, ns_id, ns_slug, page_slug, title)| {
            Ok(BacklinkRow {
                source_page_id: thewiki_core::PageId::from_uuid(crate::sqlite::codec::decode_uuid(
                    &id,
                )?),
                source_namespace_id: NamespaceId::from_uuid(crate::sqlite::codec::decode_uuid(
                    &ns_id,
                )?),
                source_namespace_slug: ns_slug,
                source_page_slug: page_slug,
                source_page_title: title,
            })
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    Ok(PageSlice { items, next })
}
