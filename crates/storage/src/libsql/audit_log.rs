//! libsql [`AuditLogRepository`](crate::repo::AuditLogRepository) impl.

use libsql::{Connection, Value};
use thewiki_core::AuditLogId;
use time::format_description::FormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

use crate::error::StorageError;
use crate::libsql::codec::{
    audit_log_from_libsql_row, hex_decode_id, hex_encode, into_db, parse_ts, uuid_bytes,
};
use crate::repo::{
    AuditLogEntry, AuditLogFilter, AuditLogRepository, Cursor, NewAuditLogEntry, PageSlice,
    clamp_limit,
};

/// Subsecond-precision RFC 3339 used by audit-log timestamps. Same shape as
/// the SQLite adapter so rows produced by either backend compare as expected.
const AUDIT_TS_FORMAT: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:9]Z");

/// libsql-backed audit-log repository.
pub struct LibsqlAuditLogRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LibsqlAuditLogRepository<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl AuditLogRepository for LibsqlAuditLogRepository<'_> {
    async fn create(&self, entry: NewAuditLogEntry) -> Result<AuditLogEntry, StorageError> {
        let id = AuditLogId::new();
        let created_at = OffsetDateTime::now_utc();
        let id_bytes = uuid_bytes(id.into_uuid());
        let actor_bytes = uuid_bytes(entry.actor_id.into_uuid());
        let target_bytes = uuid_bytes(entry.target_id);
        let created_at_str = format_audit_ts(created_at)?;
        let metadata = serde_json::to_string(&entry.metadata)
            .map_err(|err| StorageError::invalid_input(format!("audit metadata: {err}")))?;

        let binds: Vec<Value> = vec![
            Value::Blob(id_bytes.to_vec()),
            Value::Blob(actor_bytes.to_vec()),
            Value::Text(entry.actor_username.clone()),
            Value::Text(entry.action.clone()),
            Value::Text(entry.target_kind.clone()),
            Value::Blob(target_bytes.to_vec()),
            match entry.target_label.as_deref() {
                Some(s) => Value::Text(s.to_owned()),
                None => Value::Null,
            },
            Value::Text(metadata),
            Value::Text(created_at_str),
        ];

        into_db(
            self.conn
                .execute(
                    "INSERT INTO audit_log \
                     (id, actor_id, actor_username, action, target_kind, target_id, target_label, metadata, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    binds,
                )
                .await,
        )?;

        Ok(AuditLogEntry {
            id,
            actor_id: entry.actor_id,
            actor_username: entry.actor_username,
            action: entry.action,
            target_kind: entry.target_kind,
            target_id: entry.target_id,
            target_label: entry.target_label,
            metadata: entry.metadata,
            created_at,
        })
    }

    async fn list(
        &self,
        filter: AuditLogFilter,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<PageSlice<AuditLogEntry>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit) + 1;
        let cursor_pair = cursor.map(|c| decode_cursor(&c)).transpose()?;
        let since_str = filter.since.map(format_audit_ts).transpose()?;
        let until_str = filter.until.map(format_audit_ts).transpose()?;

        let mut sql = String::from(
            "SELECT id, actor_id, actor_username, action, target_kind, target_id, \
                    target_label, metadata, created_at \
             FROM audit_log \
             WHERE 1 = 1",
        );
        if filter.actor_username.is_some() {
            sql.push_str(" AND actor_username = ?");
        }
        if filter.action.is_some() {
            sql.push_str(" AND action = ?");
        }
        if since_str.is_some() {
            sql.push_str(" AND created_at >= ?");
        }
        if until_str.is_some() {
            sql.push_str(" AND created_at <= ?");
        }
        if cursor_pair.is_some() {
            sql.push_str(" AND (created_at, id) < (?, ?)");
        }
        sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");

        let mut binds: Vec<Value> = Vec::new();
        if let Some(actor) = filter.actor_username.as_ref() {
            binds.push(Value::Text(actor.clone()));
        }
        if let Some(action) = filter.action.as_ref() {
            binds.push(Value::Text(action.clone()));
        }
        if let Some(s) = since_str {
            binds.push(Value::Text(s));
        }
        if let Some(u) = until_str {
            binds.push(Value::Text(u));
        }
        if let Some((ts, id_hex)) = cursor_pair.as_ref() {
            let id_bytes = hex_decode_id(id_hex)?;
            binds.push(Value::Text(ts.clone()));
            binds.push(Value::Blob(id_bytes.to_vec()));
        }
        binds.push(Value::Integer(take));

        let mut rows = into_db(self.conn.query(&sql, binds).await)?;
        let mut collected: Vec<(String, [u8; 16], AuditLogEntry)> =
            Vec::with_capacity(limit as usize + 1);
        while let Some(row) = into_db(rows.next().await)? {
            let id_blob: Vec<u8> = into_db(row.get::<Vec<u8>>(0))?;
            let id_arr: [u8; 16] = id_blob
                .as_slice()
                .try_into()
                .map_err(|_| StorageError::invalid_input("audit id column wrong size"))?;
            let created_at: String = into_db(row.get::<String>(8))?;
            let entry = audit_log_from_libsql_row(&row)?;
            collected.push((created_at, id_arr, entry));
        }
        finalise(collected, limit)
    }

    async fn prune_before(&self, cutoff: OffsetDateTime) -> Result<u64, StorageError> {
        let cutoff = format_audit_ts(cutoff)?;
        into_db(
            self.conn
                .execute(
                    "DELETE FROM audit_log WHERE created_at < ?1",
                    vec![Value::Text(cutoff)],
                )
                .await,
        )
    }
}

fn encode_cursor(created_at: &str, id: &[u8]) -> Cursor {
    Cursor(format!("{}|{}", created_at, hex_encode(id)))
}

fn decode_cursor(c: &Cursor) -> Result<(String, String), StorageError> {
    let (ts, id_hex) = c.0.split_once('|').ok_or_else(|| {
        StorageError::invalid_input("audit-log cursor must be `<timestamp>|<hex-id>`")
    })?;
    let ts = format_audit_ts(parse_ts(ts)?)?;
    Ok((ts, id_hex.to_string()))
}

pub(super) fn format_audit_ts(ts: OffsetDateTime) -> Result<String, StorageError> {
    ts.to_offset(UtcOffset::UTC)
        .format(AUDIT_TS_FORMAT)
        .map_err(|err| StorageError::invalid_input(format!("could not format timestamp: {err}")))
}

fn finalise(
    mut rows: Vec<(String, [u8; 16], AuditLogEntry)>,
    limit: u32,
) -> Result<PageSlice<AuditLogEntry>, StorageError> {
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
