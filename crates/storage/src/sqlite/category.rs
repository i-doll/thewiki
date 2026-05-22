//! SQLite [`CategoryRepository`](crate::repo::CategoryRepository) impl (#29).
//!
//! The category set is intentionally small (operator-curated taxonomy) so
//! `list_all` doesn't bother paginating. The page-membership query (`list_pages_in`)
//! does paginate because a category can hold thousands of pages.
//!
//! Cycle prevention lives at this layer: every mutation that points a
//! `parent_id` at another category first walks that target's ancestor chain
//! and rejects the write if the current node already appears in it. Without
//! the check a careless caller could create a cycle in the supposedly-DAG
//! shape; the database has no `CHECK` that walks a recursive graph.

use sqlx::SqlitePool;
use thewiki_core::{Category, CategoryId, NamespaceId, PageId};

use crate::error::StorageError;
use crate::repo::{CategoryRepository, Cursor, PageMemberRow, PageSlice, clamp_limit};
use crate::sqlite::codec::{
    decode_uuid, format_ts, hex_decode_id, hex_encode, map_unique_violation, parse_ts, uuid_bytes,
};

/// Shape of a `categories` row coming back from the driver.
type CategoryRow = (
    Vec<u8>,         // id
    String,          // slug
    String,          // display_name
    Option<Vec<u8>>, // parent_id
    String,          // created_at
);

/// Shape of the JOIN row produced by `list_pages_in`: page-id, namespace-id,
/// namespace slug, page slug, page title.
type MemberJoinRow = (Vec<u8>, Vec<u8>, String, String, String);

fn row_to_category(row: CategoryRow) -> Result<Category, StorageError> {
    let (id, slug, display_name, parent_id, created_at) = row;
    Ok(Category {
        id: CategoryId::from_uuid(decode_uuid(&id)?),
        slug,
        display_name,
        parent_id: parent_id
            .as_deref()
            .map(decode_uuid)
            .transpose()?
            .map(CategoryId::from_uuid),
        created_at: parse_ts(&created_at)?,
    })
}

/// SQLite-backed category repository.
pub struct SqliteCategoryRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteCategoryRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

/// Walk the parent chain of `start` and collect every ancestor's id.
///
/// Used by [`SqliteCategoryRepository::create`] to ensure a freshly-minted
/// category's `parent_id` cannot reference an ancestor of itself. Returns an
/// empty set when `start` has no parent.
async fn ancestor_ids(
    pool: &SqlitePool,
    start: CategoryId,
) -> Result<Vec<CategoryId>, StorageError> {
    let mut out: Vec<CategoryId> = Vec::new();
    let mut current: Option<CategoryId> = Some(start);
    // Guard against malformed DBs (a pre-existing cycle would otherwise spin
    // forever). 256 hops is wildly above any realistic taxonomy depth.
    let mut budget = 256u32;
    while let Some(id) = current {
        if budget == 0 {
            return Err(StorageError::invalid_input(
                "category ancestor walk exceeded depth budget (cycle in storage?)",
            ));
        }
        budget -= 1;
        let bytes = uuid_bytes(id.into_uuid());
        let row: Option<(Option<Vec<u8>>,)> =
            sqlx::query_as("SELECT parent_id FROM categories WHERE id = ?1")
                .bind(bytes.as_slice())
                .fetch_optional(pool)
                .await?;
        match row {
            Some((Some(parent_bytes),)) => {
                let parent = CategoryId::from_uuid(decode_uuid(&parent_bytes)?);
                out.push(parent);
                current = Some(parent);
            }
            // Either the row doesn't exist (NotFound bubbles to the caller
            // via the eventual `get_by_id` call) or the parent is NULL —
            // either way the walk is over.
            Some((None,)) | None => current = None,
        }
    }
    Ok(out)
}

impl CategoryRepository for SqliteCategoryRepository<'_> {
    async fn create(&self, category: &Category) -> Result<(), StorageError> {
        // Cycle check: a fresh category can't point at any of its own
        // ancestors. The freshly-minted UUID can't be an ancestor of an
        // existing row, so the only thing we have to check is that the
        // proposed `parent_id` actually resolves to something — the
        // ancestor walk handles "parent doesn't exist" too (an empty
        // ancestor chain for a never-seen-before id), so we follow up with
        // an explicit existence probe.
        if let Some(parent) = category.parent_id {
            let parent_bytes = uuid_bytes(parent.into_uuid());
            let parent_exists: Option<(i64,)> =
                sqlx::query_as("SELECT 1 FROM categories WHERE id = ?1")
                    .bind(parent_bytes.as_slice())
                    .fetch_optional(self.pool)
                    .await?;
            if parent_exists.is_none() {
                return Err(StorageError::NotFound);
            }
            // Defence in depth — if the storage backed cycle prevention
            // ever slips, the explicit "parent must not list `category.id`
            // among its own ancestors" check catches it.
            let ancestors = ancestor_ids(self.pool, parent).await?;
            if ancestors.contains(&category.id) {
                return Err(StorageError::Conflict(
                    "assigning this parent would create a category cycle".into(),
                ));
            }
        }

        let id = uuid_bytes(category.id.into_uuid());
        let parent_bytes = category.parent_id.map(|p| uuid_bytes(p.into_uuid()));
        let created_at = format_ts(category.created_at)?;
        let result = sqlx::query(
            "INSERT INTO categories (id, slug, display_name, parent_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(id.as_slice())
        .bind(&category.slug)
        .bind(&category.display_name)
        .bind(parent_bytes.as_ref().map(|b| b.as_slice()))
        .bind(&created_at)
        .execute(self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(err, "category slug already in use")),
        }
    }

    async fn get_by_id(&self, id: CategoryId) -> Result<Category, StorageError> {
        let bytes = uuid_bytes(id.into_uuid());
        let row: Option<CategoryRow> = sqlx::query_as(
            "SELECT id, slug, display_name, parent_id, created_at
             FROM categories WHERE id = ?1",
        )
        .bind(bytes.as_slice())
        .fetch_optional(self.pool)
        .await?;
        match row {
            Some(row) => row_to_category(row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_slug(&self, slug: &str) -> Result<Category, StorageError> {
        let row: Option<CategoryRow> = sqlx::query_as(
            "SELECT id, slug, display_name, parent_id, created_at
             FROM categories WHERE slug = ?1",
        )
        .bind(slug)
        .fetch_optional(self.pool)
        .await?;
        match row {
            Some(row) => row_to_category(row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn list_all(&self) -> Result<Vec<Category>, StorageError> {
        let rows: Vec<CategoryRow> = sqlx::query_as(
            "SELECT id, slug, display_name, parent_id, created_at
             FROM categories ORDER BY slug ASC",
        )
        .fetch_all(self.pool)
        .await?;
        rows.into_iter().map(row_to_category).collect()
    }

    async fn list_children(
        &self,
        parent: Option<CategoryId>,
    ) -> Result<Vec<Category>, StorageError> {
        let rows: Vec<CategoryRow> = if let Some(parent) = parent {
            let bytes = uuid_bytes(parent.into_uuid());
            sqlx::query_as(
                "SELECT id, slug, display_name, parent_id, created_at
                 FROM categories
                 WHERE parent_id = ?1
                 ORDER BY slug ASC",
            )
            .bind(bytes.as_slice())
            .fetch_all(self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT id, slug, display_name, parent_id, created_at
                 FROM categories
                 WHERE parent_id IS NULL
                 ORDER BY slug ASC",
            )
            .fetch_all(self.pool)
            .await?
        };
        rows.into_iter().map(row_to_category).collect()
    }

    async fn list_ancestors(&self, id: CategoryId) -> Result<Vec<Category>, StorageError> {
        // Confirm `id` exists; the ancestor walk silently terminates on
        // missing rows so we'd otherwise return an empty list for a bogus
        // id, which is the wrong semantic.
        let _ = self.get_by_id(id).await?;
        let ancestor_ids = ancestor_ids(self.pool, id).await?;
        let mut out: Vec<Category> = Vec::with_capacity(ancestor_ids.len());
        for ancestor in ancestor_ids {
            out.push(self.get_by_id(ancestor).await?);
        }
        Ok(out)
    }

    async fn assign_to_page(
        &self,
        page_id: PageId,
        category_id: CategoryId,
    ) -> Result<(), StorageError> {
        let page = uuid_bytes(page_id.into_uuid());
        let category = uuid_bytes(category_id.into_uuid());
        sqlx::query(
            "INSERT OR IGNORE INTO page_categories (page_id, category_id)
             VALUES (?1, ?2)",
        )
        .bind(page.as_slice())
        .bind(category.as_slice())
        .execute(self.pool)
        .await?;
        Ok(())
    }

    async fn unassign_from_page(
        &self,
        page_id: PageId,
        category_id: CategoryId,
    ) -> Result<(), StorageError> {
        let page = uuid_bytes(page_id.into_uuid());
        let category = uuid_bytes(category_id.into_uuid());
        sqlx::query("DELETE FROM page_categories WHERE page_id = ?1 AND category_id = ?2")
            .bind(page.as_slice())
            .bind(category.as_slice())
            .execute(self.pool)
            .await?;
        Ok(())
    }

    async fn replace_for_page(
        &self,
        page_id: PageId,
        categories: &[CategoryId],
    ) -> Result<(), StorageError> {
        let page = uuid_bytes(page_id.into_uuid());
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM page_categories WHERE page_id = ?1")
            .bind(page.as_slice())
            .execute(&mut *tx)
            .await?;
        for category_id in categories {
            let category = uuid_bytes(category_id.into_uuid());
            sqlx::query(
                "INSERT OR IGNORE INTO page_categories (page_id, category_id)
                 VALUES (?1, ?2)",
            )
            .bind(page.as_slice())
            .bind(category.as_slice())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn list_for_page(&self, page_id: PageId) -> Result<Vec<Category>, StorageError> {
        let page = uuid_bytes(page_id.into_uuid());
        let rows: Vec<CategoryRow> = sqlx::query_as(
            "SELECT c.id, c.slug, c.display_name, c.parent_id, c.created_at
             FROM page_categories pc
             JOIN categories c ON c.id = pc.category_id
             WHERE pc.page_id = ?1
             ORDER BY c.slug ASC",
        )
        .bind(page.as_slice())
        .fetch_all(self.pool)
        .await?;
        rows.into_iter().map(row_to_category).collect()
    }

    async fn list_pages_in(
        &self,
        category_id: CategoryId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<PageSlice<PageMemberRow>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit) + 1;
        let category = uuid_bytes(category_id.into_uuid());

        let rows: Vec<MemberJoinRow> = if let Some(cursor) = cursor {
            let id_bytes = hex_decode_id(cursor.as_str())?;
            sqlx::query_as(
                "SELECT pages.id, pages.namespace_id, namespaces.slug,
                        pages.slug, pages.title
                 FROM page_categories
                 JOIN pages      ON pages.id      = page_categories.page_id
                 JOIN namespaces ON namespaces.id = pages.namespace_id
                 WHERE page_categories.category_id = ?1
                   AND pages.id > ?2
                 ORDER BY pages.id ASC
                 LIMIT ?3",
            )
            .bind(category.as_slice())
            .bind(id_bytes.as_slice())
            .bind(take)
            .fetch_all(self.pool)
            .await?
        } else {
            sqlx::query_as(
                "SELECT pages.id, pages.namespace_id, namespaces.slug,
                        pages.slug, pages.title
                 FROM page_categories
                 JOIN pages      ON pages.id      = page_categories.page_id
                 JOIN namespaces ON namespaces.id = pages.namespace_id
                 WHERE page_categories.category_id = ?1
                 ORDER BY pages.id ASC
                 LIMIT ?2",
            )
            .bind(category.as_slice())
            .bind(take)
            .fetch_all(self.pool)
            .await?
        };

        finalise_members(rows, limit)
    }
}

fn finalise_members(
    mut rows: Vec<MemberJoinRow>,
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
