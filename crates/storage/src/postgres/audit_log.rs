//! Postgres [`AuditLogRepository`](crate::repo::AuditLogRepository) impl.
//!
//! `metadata` lands in a native `JSONB` column, so we bind / fetch
//! [`serde_json::Value`] directly rather than serialising through a TEXT
//! column. The cursor remains `<rfc3339-timestamp>|<hyphenated-uuid>` for
//! parity with the SQLite adapter.

use std::fmt::Write;

use serde_json::Value;
use sqlx::PgPool;
use thewiki_core::{AuditLogId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::StorageError;
use crate::postgres::codec::{format_cursor_ts, parse_cursor_ts};
use crate::repo::{
    AuditLogEntry, AuditLogFilter, AuditLogRepository, Cursor, NewAuditLogEntry, PageSlice,
    clamp_limit,
};

type Row = (
    Uuid,           // id
    Uuid,           // actor_id
    String,         // actor_username
    String,         // action
    String,         // target_kind
    Uuid,           // target_id
    Option<String>, // target_label
    Value,          // metadata
    OffsetDateTime, // created_at
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
    Ok(AuditLogEntry {
        id: AuditLogId::from_uuid(id),
        actor_id: UserId::from_uuid(actor_id),
        actor_username,
        action,
        target_kind,
        target_id,
        target_label,
        metadata,
        created_at,
    })
}

/// Postgres-backed audit-log repository.
pub struct PostgresAuditLogRepository<'a> {
    pool: &'a PgPool,
}

impl<'a> PostgresAuditLogRepository<'a> {
    pub(super) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }
}

impl AuditLogRepository for PostgresAuditLogRepository<'_> {
    async fn create(&self, entry: NewAuditLogEntry) -> Result<AuditLogEntry, StorageError> {
        let id = AuditLogId::new();
        let created_at = OffsetDateTime::now_utc();

        sqlx::query(
            "INSERT INTO audit_log \
             (id, actor_id, actor_username, action, target_kind, target_id, target_label, metadata, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id.into_uuid())
        .bind(entry.actor_id.into_uuid())
        .bind(&entry.actor_username)
        .bind(&entry.action)
        .bind(&entry.target_kind)
        .bind(entry.target_id)
        .bind(&entry.target_label)
        .bind(&entry.metadata)
        .bind(created_at)
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

        let mut sql = String::from(
            "SELECT id, actor_id, actor_username, action, target_kind, target_id, \
                    target_label, metadata, created_at \
             FROM audit_log \
             WHERE 1 = 1",
        );
        let mut idx: i32 = 1;
        let next_param = |sql: &mut String, fragment: &str, idx: &mut i32| {
            let _ = write!(sql, " {fragment} ${idx}");
            *idx += 1;
        };
        if filter.actor_username.is_some() {
            next_param(&mut sql, "AND actor_username =", &mut idx);
        }
        if filter.action.is_some() {
            next_param(&mut sql, "AND action =", &mut idx);
        }
        if filter.since.is_some() {
            next_param(&mut sql, "AND created_at >=", &mut idx);
        }
        if filter.until.is_some() {
            next_param(&mut sql, "AND created_at <=", &mut idx);
        }
        if cursor_pair.is_some() {
            let _ = write!(sql, " AND (created_at, id) < (${idx}, ${})", idx + 1);
            idx += 2;
        }
        let _ = write!(sql, " ORDER BY created_at DESC, id DESC LIMIT ${idx}");

        let mut query = sqlx::query_as::<_, Row>(&sql);
        if let Some(actor) = filter.actor_username.as_ref() {
            query = query.bind(actor);
        }
        if let Some(action) = filter.action.as_ref() {
            query = query.bind(action);
        }
        if let Some(since) = filter.since {
            query = query.bind(since);
        }
        if let Some(until) = filter.until {
            query = query.bind(until);
        }
        if let Some((ts, id)) = cursor_pair {
            query = query.bind(ts).bind(id);
        }
        query = query.bind(take);

        let rows = query.fetch_all(self.pool).await?;
        finalise_page(rows, limit)
    }

    async fn prune_before(&self, cutoff: OffsetDateTime) -> Result<u64, StorageError> {
        let result = sqlx::query("DELETE FROM audit_log WHERE created_at < $1")
            .bind(cutoff)
            .execute(self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

fn encode_cursor(created_at: OffsetDateTime, id: Uuid) -> Result<Cursor, StorageError> {
    Ok(Cursor(format!("{}|{}", format_cursor_ts(created_at)?, id)))
}

fn decode_cursor(c: &Cursor) -> Result<(OffsetDateTime, Uuid), StorageError> {
    let (ts, id_str) = c.0.split_once('|').ok_or_else(|| {
        StorageError::invalid_input("audit-log cursor must be `<timestamp>|<uuid>`")
    })?;
    let ts = parse_cursor_ts(ts)?;
    let id = Uuid::parse_str(id_str)
        .map_err(|err| StorageError::invalid_input(format!("audit-log cursor uuid: {err}")))?;
    Ok((ts, id))
}

fn finalise_page(mut rows: Vec<Row>, limit: u32) -> Result<PageSlice<AuditLogEntry>, StorageError> {
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
        .map(row_to_entry)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PageSlice { items, next })
}
