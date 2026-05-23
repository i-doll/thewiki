//! libsql [`RecentChangesRepository`](crate::repo::RecentChangesRepository)
//! impl.
//!
//! Mirror of the SQLite adapter — the JOIN over `revisions × pages ×
//! namespaces × users` is identical SQL, only the driver call sites differ.

use libsql::{Connection, Value};

use crate::error::StorageError;
use crate::libsql::codec::{
    format_ts, hex_decode_id, hex_encode, into_db, parse_ts, recent_change_from_libsql_row,
    uuid_bytes,
};
use crate::repo::{
    Cursor, PageSlice, RecentChange, RecentChangesFilter, RecentChangesRepository, clamp_limit,
};

/// libsql-backed recent-changes repository.
pub struct LibsqlRecentChangesRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LibsqlRecentChangesRepository<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl RecentChangesRepository for LibsqlRecentChangesRepository<'_> {
    async fn list(
        &self,
        filter: RecentChangesFilter,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<PageSlice<RecentChange>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit) + 1;

        let cursor_pair = cursor.map(|c| decode_cursor(&c)).transpose()?;

        let since_str = filter.since.map(format_ts).transpose()?;
        let ns_bytes = filter.namespace_id.map(|n| uuid_bytes(n.into_uuid()));
        let actor_bytes = filter.actor_id.map(|u| uuid_bytes(u.into_uuid()));

        // Build the SQL once. `?` is positional; we bind in the same order we
        // appended the predicates.
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
        if filter.public_only {
            // See SQLite sibling for rationale: push the protection filter down
            // so `LIMIT` counts public rows only.
            sql.push_str(" AND p.protection_level IN ('none', 'semi_protected')");
        }
        if cursor_pair.is_some() {
            sql.push_str(" AND (r.created_at, r.id) < (?, ?)");
        }
        sql.push_str(" ORDER BY r.created_at DESC, r.id DESC LIMIT ?");

        // libsql params are passed as a `Vec<Value>` for positional sets.
        let mut binds: Vec<Value> = Vec::new();
        if let Some(s) = since_str {
            binds.push(Value::Text(s));
        }
        if let Some(n) = ns_bytes {
            binds.push(Value::Blob(n.to_vec()));
        }
        if let Some(a) = actor_bytes {
            binds.push(Value::Blob(a.to_vec()));
        }
        if let Some((ts, id_hex)) = cursor_pair.as_ref() {
            let id_bytes = hex_decode_id(id_hex)?;
            binds.push(Value::Text(ts.clone()));
            binds.push(Value::Blob(id_bytes.to_vec()));
        }
        binds.push(Value::Integer(take));

        let mut rows = into_db(self.conn.query(&sql, binds).await)?;
        let mut collected: Vec<(String, [u8; 16], RecentChange)> =
            Vec::with_capacity(limit as usize + 1);
        while let Some(row) = into_db(rows.next().await)? {
            let id_blob: Vec<u8> = into_db(row.get::<Vec<u8>>(0))?;
            let id_arr: [u8; 16] = id_blob
                .as_slice()
                .try_into()
                .map_err(|_| StorageError::invalid_input("revision id column wrong size"))?;
            let created_at: String = into_db(row.get::<String>(8))?;
            let entry = recent_change_from_libsql_row(&row)?;
            collected.push((created_at, id_arr, entry));
        }
        finalise(collected, limit)
    }
}

fn encode_cursor(created_at: &str, id: &[u8]) -> Cursor {
    Cursor(format!("{}|{}", created_at, hex_encode(id)))
}

fn decode_cursor(c: &Cursor) -> Result<(String, String), StorageError> {
    let (ts, id_hex) = c.0.split_once('|').ok_or_else(|| {
        StorageError::invalid_input("recent-changes cursor must be `<timestamp>|<hex-id>`")
    })?;
    // Validate the timestamp half so a malformed cursor surfaces as
    // `InvalidInput` rather than silently degrading to TEXT comparison.
    parse_ts(ts)?;
    Ok((ts.to_string(), id_hex.to_string()))
}

fn finalise(
    mut rows: Vec<(String, [u8; 16], RecentChange)>,
    limit: u32,
) -> Result<PageSlice<RecentChange>, StorageError> {
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
    let items = rows.into_iter().map(|(_, _, e)| e).collect();
    Ok(PageSlice { items, next })
}
