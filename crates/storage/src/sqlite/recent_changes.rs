//! SQLite [`RecentChangesRepository`](crate::repo::RecentChangesRepository) impl.
//!
//! The recent-changes feed is a chronological view across the wiki: every
//! [`Revision`](thewiki_core::Revision) joined to its [`Page`](thewiki_core::Page),
//! [`Namespace`](thewiki_core::Namespace), and [`User`](thewiki_core::User) so the
//! API can hand back a fully hydrated row without follow-up lookups.
//!
//! The query is parameterised by [`RecentChangesFilter`] — any subset of
//! `since` / `namespace_id` / `actor_id` may be set; absent filters drop the
//! corresponding `WHERE` predicate. Ordering is `(created_at DESC, id DESC)`
//! and the cursor encodes the last returned `(created_at, id)` so that
//! pagination remains stable when new edits land between calls.

use sqlx::SqlitePool;
use thewiki_core::{NamespaceId, PageId, RevisionId, UserId};

use crate::error::StorageError;
use crate::repo::{
    Cursor, PageSlice, RecentChange, RecentChangesFilter, RecentChangesRepository, clamp_limit,
};
use crate::sqlite::codec::{format_ts, hex_decode_id, hex_encode, parse_ts, uuid_bytes};

/// Shape of a recent-changes row coming back from the driver.
type Row = (
    Vec<u8>,        // r.id
    Vec<u8>,        // r.page_id
    String,         // p.slug
    Vec<u8>,        // p.namespace_id
    String,         // n.slug
    Vec<u8>,        // r.author_id
    String,         // u.username
    Option<String>, // r.edit_summary
    String,         // r.created_at
    String,         // p.protection_level
);

fn row_to_recent_change(row: Row) -> Result<RecentChange, StorageError> {
    let (
        revision_id,
        page_id,
        page_slug,
        namespace_id,
        namespace_slug,
        author_id,
        author_username,
        edit_summary,
        created_at,
        protection_level,
    ) = row;
    Ok(RecentChange {
        revision_id: RevisionId::from_uuid(crate::sqlite::codec::decode_uuid(&revision_id)?),
        page_id: PageId::from_uuid(crate::sqlite::codec::decode_uuid(&page_id)?),
        page_slug,
        namespace_id: NamespaceId::from_uuid(crate::sqlite::codec::decode_uuid(&namespace_id)?),
        namespace_slug,
        author_id: UserId::from_uuid(crate::sqlite::codec::decode_uuid(&author_id)?),
        author_username,
        edit_summary,
        created_at: parse_ts(&created_at)?,
        protection_level: crate::codec::parse_protection_level(&protection_level)?,
    })
}

/// SQLite-backed recent-changes repository.
pub struct SqliteRecentChangesRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteRecentChangesRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl RecentChangesRepository for SqliteRecentChangesRepository<'_> {
    async fn list(
        &self,
        filter: RecentChangesFilter,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<PageSlice<RecentChange>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit) + 1;

        // Decode the cursor up front so a malformed token fails fast as
        // `InvalidInput` rather than poisoning the SQL.
        let cursor_pair = cursor.map(|c| decode_cursor(&c)).transpose()?;

        // Translate filter fields once. Holding the binary blob owners on the
        // stack keeps the `bind` lifetimes straight.
        let since_str = filter.since.map(format_ts).transpose()?;
        let ns_bytes = filter.namespace_id.map(|n| uuid_bytes(n.into_uuid()));
        let actor_bytes = filter.actor_id.map(|u| uuid_bytes(u.into_uuid()));

        // Build the SQL dynamically — we'd rather have one query with a
        // variable predicate set than four. `?N` placeholders are positional
        // and we bind in the same order we appended them.
        let mut sql = String::from(
            "SELECT r.id, r.page_id, p.slug, p.namespace_id, n.slug, r.author_id, \
                    u.username, r.edit_summary, r.created_at, p.protection_level \
             FROM revisions r \
             JOIN pages p      ON r.page_id      = p.id \
             JOIN namespaces n ON p.namespace_id = n.id \
             JOIN users u      ON r.author_id    = u.id \
             WHERE 1 = 1",
        );

        if since_str.is_some() {
            sql.push_str(" AND r.created_at >= ?");
        }
        if ns_bytes.is_some() {
            sql.push_str(" AND p.namespace_id = ?");
        }
        if actor_bytes.is_some() {
            sql.push_str(" AND r.author_id = ?");
        }
        if cursor_pair.is_some() {
            // Newest-first means "strictly older" than the cursor. Row-value
            // comparison evaluates element-by-element by column affinity; the
            // `created_at` half is TEXT (RFC3339, lexicographically sortable)
            // and the `id` half is BLOB(16) (bytewise compared). Both halves
            // are total orders, so the predicate is stable even when two
            // revisions share a timestamp.
            sql.push_str(" AND (r.created_at, r.id) < (?, ?)");
        }
        sql.push_str(" ORDER BY r.created_at DESC, r.id DESC LIMIT ?");

        let mut query = sqlx::query_as::<_, Row>(&sql);
        if let Some(s) = since_str.as_ref() {
            query = query.bind(s);
        }
        if let Some(n) = ns_bytes.as_ref() {
            query = query.bind(n.as_slice());
        }
        if let Some(a) = actor_bytes.as_ref() {
            query = query.bind(a.as_slice());
        }
        let cursor_id_bytes = if let Some((ts, id_hex)) = cursor_pair.as_ref() {
            let id_bytes = hex_decode_id(id_hex)?;
            query = query.bind(ts);
            Some(id_bytes)
        } else {
            None
        };
        if let Some(id_bytes) = cursor_id_bytes.as_ref() {
            query = query.bind(id_bytes.as_slice());
        }
        query = query.bind(take);

        let rows: Vec<Row> = query.fetch_all(self.pool).await?;
        finalise_page(rows, limit)
    }
}

/// Encode a `(created_at, id)` pair as the opaque cursor string we hand
/// back to callers.
fn encode_cursor(created_at: &str, id: &[u8]) -> Cursor {
    Cursor(format!("{}|{}", created_at, hex_encode(id)))
}

fn decode_cursor(c: &Cursor) -> Result<(String, String), StorageError> {
    let (ts, id_hex) = c.0.split_once('|').ok_or_else(|| {
        StorageError::invalid_input("recent-changes cursor must be `<timestamp>|<hex-id>`")
    })?;
    // Validate the timestamp half so a malformed cursor surfaces as
    // `InvalidInput` rather than silently degrading to TEXT comparison and
    // producing wrong results.
    parse_ts(ts)?;
    Ok((ts.to_string(), id_hex.to_string()))
}

fn finalise_page(mut rows: Vec<Row>, limit: u32) -> Result<PageSlice<RecentChange>, StorageError> {
    let limit_usize = limit as usize;
    let has_more = rows.len() > limit_usize;
    if has_more {
        rows.truncate(limit_usize);
    }
    // The cursor anchors at the LAST returned row so the next page resumes
    // strictly older than it.
    let next = if has_more {
        rows.last().map(|last| encode_cursor(&last.8, &last.0))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(row_to_recent_change)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PageSlice { items, next })
}
