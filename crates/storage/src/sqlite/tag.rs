//! SQLite [`TagRepository`](crate::repo::TagRepository) impl (#29).
//!
//! Tags are stored as lowercased TEXT (the [`Tag`] newtype guarantees the
//! incoming value is already normalised); the autocomplete endpoint binds
//! a user-typed prefix as a `LIKE` pattern.

use sqlx::SqlitePool;
use thewiki_core::{NamespaceId, PageId, Tag};

use crate::error::StorageError;
use crate::repo::{Cursor, PageMemberRow, PageSlice, TagRepository, clamp_limit};
use crate::sqlite::codec::{decode_uuid, hex_decode_id, hex_encode, uuid_bytes};

/// Shape of the JOIN row produced by `list_pages_with_tag`: page-id,
/// namespace-id, namespace slug, page slug, page title.
type TagJoinRow = (Vec<u8>, Vec<u8>, String, String, String);

/// SQLite-backed tag repository.
pub struct SqliteTagRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteTagRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl TagRepository for SqliteTagRepository<'_> {
    async fn assign(&self, page_id: PageId, tags: &[Tag]) -> Result<(), StorageError> {
        let page = uuid_bytes(page_id.into_uuid());
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM page_tags WHERE page_id = ?1")
            .bind(page.as_slice())
            .execute(&mut *tx)
            .await?;
        for tag in tags {
            sqlx::query("INSERT OR IGNORE INTO page_tags (page_id, tag) VALUES (?1, ?2)")
                .bind(page.as_slice())
                .bind(tag.as_str())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn list_for_page(&self, page_id: PageId) -> Result<Vec<Tag>, StorageError> {
        let page = uuid_bytes(page_id.into_uuid());
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT tag FROM page_tags WHERE page_id = ?1 ORDER BY tag ASC")
                .bind(page.as_slice())
                .fetch_all(self.pool)
                .await?;
        rows.into_iter()
            .map(|(t,)| {
                Tag::new(t).map_err(|err| {
                    StorageError::invalid_input(format!("stored tag invalid: {err}"))
                })
            })
            .collect()
    }

    async fn list_pages_with_tag(
        &self,
        tag: &Tag,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<PageSlice<PageMemberRow>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit) + 1;

        let rows: Vec<TagJoinRow> = if let Some(cursor) = cursor {
            let id_bytes = hex_decode_id(cursor.as_str())?;
            sqlx::query_as(
                "SELECT pages.id, pages.namespace_id, namespaces.slug,
                        pages.slug, pages.title
                 FROM page_tags
                 JOIN pages      ON pages.id      = page_tags.page_id
                 JOIN namespaces ON namespaces.id = pages.namespace_id
                 WHERE page_tags.tag = ?1
                   AND pages.id > ?2
                 ORDER BY pages.id ASC
                 LIMIT ?3",
            )
            .bind(tag.as_str())
            .bind(id_bytes.as_slice())
            .bind(take)
            .fetch_all(self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT pages.id, pages.namespace_id, namespaces.slug,
                        pages.slug, pages.title
                 FROM page_tags
                 JOIN pages      ON pages.id      = page_tags.page_id
                 JOIN namespaces ON namespaces.id = pages.namespace_id
                 WHERE page_tags.tag = ?1
                 ORDER BY pages.id ASC
                 LIMIT ?2",
            )
            .bind(tag.as_str())
            .bind(take)
            .fetch_all(self.pool)
            .await?
        };

        finalise(rows, limit)
    }

    async fn list_all_tags(&self, prefix: &str, limit: u32) -> Result<Vec<Tag>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit);
        let normalised = prefix.to_ascii_lowercase();
        // Use `escape` to keep `%` / `_` from being interpreted as LIKE
        // wildcards from caller-supplied prefix input.
        let escaped = escape_like(&normalised);
        let pattern = format!("{escaped}%");

        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT tag FROM page_tags
             WHERE tag LIKE ?1 ESCAPE '\\'
             ORDER BY tag ASC
             LIMIT ?2",
        )
        .bind(pattern)
        .bind(take)
        .fetch_all(self.pool)
        .await?;
        rows.into_iter()
            .map(|(t,)| {
                Tag::new(t).map_err(|err| {
                    StorageError::invalid_input(format!("stored tag invalid: {err}"))
                })
            })
            .collect()
    }
}

fn finalise(
    mut rows: Vec<TagJoinRow>,
    limit: u32,
) -> Result<PageSlice<PageMemberRow>, StorageError> {
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
        .map(|(id, ns_id, ns_slug, slug, title)| {
            Ok(PageMemberRow {
                page_id: PageId::from_uuid(decode_uuid(&id)?),
                namespace_id: NamespaceId::from_uuid(decode_uuid(&ns_id)?),
                namespace_slug: ns_slug,
                page_slug: slug,
                page_title: title,
            })
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    Ok(PageSlice { items, next })
}

/// Escape `%`, `_`, and `\` in a LIKE pattern so caller-supplied input is
/// treated as a literal prefix rather than a wildcard.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}
