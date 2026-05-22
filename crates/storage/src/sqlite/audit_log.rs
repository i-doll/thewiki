//! SQLite [`AuditLogRepository`](crate::repo::AuditLogRepository) impl.

use serde_json::Value;
use sqlx::SqlitePool;
use thewiki_core::{AuditLogId, UserId};
use time::format_description::FormatItem;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

use crate::error::StorageError;
use crate::repo::{
    AuditLogEntry, AuditLogFilter, AuditLogRepository, Cursor, NewAuditLogEntry, PageSlice,
    clamp_limit,
};
use crate::sqlite::codec::{hex_decode_id, hex_encode, parse_ts, uuid_bytes};

const AUDIT_TS_FORMAT: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:9]Z");

type Row = (
    Vec<u8>,        // id
    Vec<u8>,        // actor_id
    String,         // actor_username
    String,         // action
    String,         // target_kind
    Vec<u8>,        // target_id
    Option<String>, // target_label
    String,         // metadata
    String,         // created_at
);

fn row_to_entry(row: Row) -> Result<AuditLogEntry, StorageError> {
    let (
        id,
        actor_id,
        actor_username,
        action,
        target_kind,
        target_id,
        target_label,
        metadata,
        created_at,
    ) = row;
    let metadata = serde_json::from_str::<Value>(&metadata).map_err(|err| {
        StorageError::Database(sqlx::Error::Protocol(format!(
            "stored audit metadata invalid: {err}"
        )))
    })?;

    Ok(AuditLogEntry {
        id: AuditLogId::from_uuid(crate::sqlite::codec::decode_uuid(&id)?),
        actor_id: UserId::from_uuid(crate::sqlite::codec::decode_uuid(&actor_id)?),
        actor_username,
        action,
        target_kind,
        target_id: crate::sqlite::codec::decode_uuid(&target_id)?,
        target_label,
        metadata,
        created_at: parse_ts(&created_at)?,
    })
}

/// SQLite-backed audit-log repository.
pub struct SqliteAuditLogRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteAuditLogRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl AuditLogRepository for SqliteAuditLogRepository<'_> {
    async fn create(&self, entry: NewAuditLogEntry) -> Result<AuditLogEntry, StorageError> {
        let id = AuditLogId::new();
        let created_at = OffsetDateTime::now_utc();
        let id_bytes = uuid_bytes(id.into_uuid());
        let actor_bytes = uuid_bytes(entry.actor_id.into_uuid());
        let target_bytes = uuid_bytes(entry.target_id);
        let created_at_str = format_audit_ts(created_at)?;
        let metadata = serde_json::to_string(&entry.metadata)
            .map_err(|err| StorageError::invalid_input(format!("audit metadata: {err}")))?;

        sqlx::query(
            "INSERT INTO audit_log \
             (id, actor_id, actor_username, action, target_kind, target_id, target_label, metadata, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(id_bytes.as_slice())
        .bind(actor_bytes.as_slice())
        .bind(&entry.actor_username)
        .bind(&entry.action)
        .bind(&entry.target_kind)
        .bind(target_bytes.as_slice())
        .bind(&entry.target_label)
        .bind(&metadata)
        .bind(&created_at_str)
        .execute(self.pool)
        .await?;

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

        let mut query = sqlx::query_as::<_, Row>(&sql);
        if let Some(actor) = filter.actor_username.as_ref() {
            query = query.bind(actor);
        }
        if let Some(action) = filter.action.as_ref() {
            query = query.bind(action);
        }
        if let Some(since) = since_str.as_ref() {
            query = query.bind(since);
        }
        if let Some(until) = until_str.as_ref() {
            query = query.bind(until);
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

        let rows = query.fetch_all(self.pool).await?;
        finalise_page(rows, limit)
    }

    async fn prune_before(&self, cutoff: OffsetDateTime) -> Result<u64, StorageError> {
        let cutoff = format_audit_ts(cutoff)?;
        let result = sqlx::query("DELETE FROM audit_log WHERE created_at < ?1")
            .bind(cutoff)
            .execute(self.pool)
            .await?;
        Ok(result.rows_affected())
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

fn finalise_page(mut rows: Vec<Row>, limit: u32) -> Result<PageSlice<AuditLogEntry>, StorageError> {
    let limit_usize = limit as usize;
    let has_more = rows.len() > limit_usize;
    if has_more {
        rows.truncate(limit_usize);
    }
    let next = if has_more {
        rows.last().map(|last| encode_cursor(&last.8, &last.0))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(row_to_entry)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PageSlice { items, next })
}
