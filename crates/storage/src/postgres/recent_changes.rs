//! Postgres [`RecentChangesRepository`](crate::repo::RecentChangesRepository) impl.
//!
//! See the SQLite sibling for the architectural notes. This impl mirrors the
//! structure: a JOIN across `revisions × pages × namespaces × users` with
//! optional filters and a `(created_at, id)` row-value cursor.
//!
//! The main wrinkle is positional placeholders. Postgres uses `$N` (1-based)
//! and rejects out-of-order numbering, so we track the next index as we
//! append predicates.

use std::fmt::Write;

use sqlx::PgPool;
use thewiki_core::{NamespaceId, PageId, RevisionId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::StorageError;
use crate::postgres::codec::{format_cursor_ts, parse_cursor_ts, parse_protection_level};
use crate::repo::{
    Cursor, PageSlice, RecentChange, RecentChangesFilter, RecentChangesRepository, clamp_limit,
};

/// Shape of a recent-changes row coming back from the driver.
type Row = (
    Uuid,           // r.id
    Uuid,           // r.page_id
    String,         // p.slug
    Uuid,           // p.namespace_id
    String,         // n.slug
    Uuid,           // r.author_id
    String,         // u.username
    Option<String>, // r.edit_summary
    OffsetDateTime, // r.created_at
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
        revision_id: RevisionId::from_uuid(revision_id),
        page_id: PageId::from_uuid(page_id),
        page_slug,
        namespace_id: NamespaceId::from_uuid(namespace_id),
        namespace_slug,
        author_id: UserId::from_uuid(author_id),
        author_username,
        edit_summary,
        created_at,
        protection_level: parse_protection_level(&protection_level)?,
    })
}

/// Postgres-backed recent-changes repository.
pub struct PostgresRecentChangesRepository<'a> {
    pool: &'a PgPool,
}

impl<'a> PostgresRecentChangesRepository<'a> {
    pub(super) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }
}

impl RecentChangesRepository for PostgresRecentChangesRepository<'_> {
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

        let since = filter.since;
        let ns_uuid = filter.namespace_id.map(|n| n.into_uuid());
        let actor_uuid = filter.actor_id.map(|u| u.into_uuid());

        // Build the SQL dynamically, tracking the next `$N` placeholder.
        let mut sql = String::from(
            "SELECT r.id, r.page_id, p.slug, p.namespace_id, n.slug, r.author_id, \
                    u.username, r.edit_summary, r.created_at, p.protection_level \
             FROM revisions r \
             JOIN pages p      ON r.page_id      = p.id \
             JOIN namespaces n ON p.namespace_id = n.id \
             JOIN users u      ON r.author_id    = u.id \
             WHERE 1 = 1",
        );
        let mut idx: i32 = 1;
        let next_param = |sql: &mut String, fragment: &str, idx: &mut i32| {
            // Single point where we materialise the `$N` placeholder; bumping
            // `idx` is part of the same statement so the caller can't forget.
            let _ = write!(sql, " {fragment} ${idx}");
            *idx += 1;
        };
        if since.is_some() {
            next_param(&mut sql, "AND r.created_at >=", &mut idx);
        }
        if ns_uuid.is_some() {
            next_param(&mut sql, "AND p.namespace_id =", &mut idx);
        }
        if actor_uuid.is_some() {
            next_param(&mut sql, "AND r.author_id =", &mut idx);
        }
        if cursor_pair.is_some() {
            // Row-value comparison resumes the descending scan strictly older
            // than the cursor.
            let _ = write!(sql, " AND (r.created_at, r.id) < (${idx}, ${})", idx + 1);
            idx += 2;
        }
        let _ = write!(sql, " ORDER BY r.created_at DESC, r.id DESC LIMIT ${idx}");

        let mut query = sqlx::query_as::<_, Row>(&sql);
        if let Some(s) = since {
            query = query.bind(s);
        }
        if let Some(n) = ns_uuid {
            query = query.bind(n);
        }
        if let Some(a) = actor_uuid {
            query = query.bind(a);
        }
        if let Some((ts, id)) = cursor_pair {
            query = query.bind(ts).bind(id);
        }
        query = query.bind(take);

        let rows: Vec<Row> = query.fetch_all(self.pool).await?;
        finalise_page(rows, limit)
    }
}

fn encode_cursor(created_at: OffsetDateTime, id: Uuid) -> Result<Cursor, StorageError> {
    Ok(Cursor(format!("{}|{}", format_cursor_ts(created_at)?, id)))
}

fn decode_cursor(c: &Cursor) -> Result<(OffsetDateTime, Uuid), StorageError> {
    let (ts, id_str) = c.0.split_once('|').ok_or_else(|| {
        StorageError::invalid_input("recent-changes cursor must be `<timestamp>|<uuid>`")
    })?;
    let ts = parse_cursor_ts(ts)?;
    let id = Uuid::parse_str(id_str)
        .map_err(|err| StorageError::invalid_input(format!("recent-changes cursor uuid: {err}")))?;
    Ok((ts, id))
}

fn finalise_page(mut rows: Vec<Row>, limit: u32) -> Result<PageSlice<RecentChange>, StorageError> {
    let limit_usize = limit as usize;
    let has_more = rows.len() > limit_usize;
    if has_more {
        rows.truncate(limit_usize);
    }
    let next = if has_more {
        match rows.last() {
            Some(last) => Some(encode_cursor(last.8, last.0)?),
            None => None,
        }
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(row_to_recent_change)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PageSlice { items, next })
}
