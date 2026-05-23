//! SQLite [`NotificationRepository`] impl (#40).
//!
//! The `payload` column stores the JSON value as a `TEXT` blob (SQLite has
//! no native JSON type); the application parses it on read.

use serde_json::Value;
use sqlx::SqlitePool;
use thewiki_core::{NewNotification, Notification, NotificationId, UserId};
use time::OffsetDateTime;

use crate::error::StorageError;
use crate::repo::{Cursor, NotificationRepository, PageSlice, clamp_limit};
use crate::sqlite::codec::{decode_uuid, format_ts, hex_decode_id, hex_encode, parse_ts, uuid_bytes};

/// Shape of one `notifications` row returned by the driver.
type Row = (
    Vec<u8>,        // id
    Vec<u8>,        // user_id
    String,         // kind
    Option<String>, // payload
    Option<String>, // read_at
    String,         // created_at
);

fn row_to_notification(row: Row) -> Result<Notification, StorageError> {
    let (id, user_id, kind, payload, read_at, created_at) = row;
    let payload = payload
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
        .map_err(|err| {
            StorageError::invalid_input(format!("stored notification payload invalid: {err}"))
        })?;
    Ok(Notification {
        id: NotificationId::from_uuid(decode_uuid(&id)?),
        user_id: UserId::from_uuid(decode_uuid(&user_id)?),
        kind,
        payload,
        read_at: read_at.as_deref().map(parse_ts).transpose()?,
        created_at: parse_ts(&created_at)?,
    })
}

/// SQLite-backed notifications repository.
pub struct SqliteNotificationRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteNotificationRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl NotificationRepository for SqliteNotificationRepository<'_> {
    async fn create(&self, new: NewNotification) -> Result<Notification, StorageError> {
        let id = NotificationId::new();
        let created_at = OffsetDateTime::now_utc();
        let id_bytes = uuid_bytes(id.into_uuid());
        let user_bytes = uuid_bytes(new.user_id.into_uuid());
        let created_at_str = format_ts(created_at)?;
        let payload_str = new
            .payload
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|err| {
                StorageError::invalid_input(format!("notification payload: {err}"))
            })?;

        sqlx::query(
            "INSERT INTO notifications (id, user_id, kind, payload, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(id_bytes.as_slice())
        .bind(user_bytes.as_slice())
        .bind(&new.kind)
        .bind(payload_str.as_deref())
        .bind(&created_at_str)
        .execute(self.pool)
        .await?;

        Ok(Notification {
            id,
            user_id: new.user_id,
            kind: new.kind,
            payload: new.payload,
            read_at: None,
            created_at,
        })
    }

    async fn list_for_user(
        &self,
        user_id: UserId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<PageSlice<Notification>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit) + 1;
        let user_bytes = uuid_bytes(user_id.into_uuid());
        let cursor_pair = cursor.map(|c| decode_cursor(&c)).transpose()?;

        let rows: Vec<Row> = match cursor_pair {
            None => {
                sqlx::query_as(
                    "SELECT id, user_id, kind, payload, read_at, created_at \
                     FROM notifications WHERE user_id = ?1 \
                     ORDER BY created_at DESC, id DESC LIMIT ?2",
                )
                .bind(user_bytes.as_slice())
                .bind(take)
                .fetch_all(self.pool)
                .await?
            }
            Some((ts, id_hex)) => {
                let cursor_id_bytes = hex_decode_id(&id_hex)?;
                sqlx::query_as(
                    "SELECT id, user_id, kind, payload, read_at, created_at \
                     FROM notifications WHERE user_id = ?1 \
                     AND (created_at < ?2 OR (created_at = ?2 AND id < ?3)) \
                     ORDER BY created_at DESC, id DESC LIMIT ?4",
                )
                .bind(user_bytes.as_slice())
                .bind(&ts)
                .bind(cursor_id_bytes.as_slice())
                .bind(take)
                .fetch_all(self.pool)
                .await?
            }
        };
        finalise_page(rows, limit)
    }

    async fn count_unread(&self, user_id: UserId) -> Result<u64, StorageError> {
        let user_bytes = uuid_bytes(user_id.into_uuid());
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM notifications WHERE user_id = ?1 AND read_at IS NULL",
        )
        .bind(user_bytes.as_slice())
        .fetch_one(self.pool)
        .await?;
        #[allow(clippy::cast_sign_loss, reason = "COUNT(*) is non-negative")]
        Ok(row.0.max(0) as u64)
    }

    async fn mark_read(
        &self,
        id: NotificationId,
        user_id: UserId,
        read_at: OffsetDateTime,
    ) -> Result<Notification, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let user_bytes = uuid_bytes(user_id.into_uuid());
        let read_at_str = format_ts(read_at)?;

        let result = sqlx::query(
            "UPDATE notifications SET read_at = COALESCE(read_at, ?1) \
             WHERE id = ?2 AND user_id = ?3",
        )
        .bind(&read_at_str)
        .bind(id_bytes.as_slice())
        .bind(user_bytes.as_slice())
        .execute(self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }

        let row: Option<Row> = sqlx::query_as(
            "SELECT id, user_id, kind, payload, read_at, created_at \
             FROM notifications WHERE id = ?1",
        )
        .bind(id_bytes.as_slice())
        .fetch_optional(self.pool)
        .await?;
        match row {
            Some(r) => row_to_notification(r),
            None => Err(StorageError::NotFound),
        }
    }
}

fn encode_cursor(created_at: &str, id: &[u8]) -> Cursor {
    Cursor(format!("{}|{}", created_at, hex_encode(id)))
}

fn decode_cursor(c: &Cursor) -> Result<(String, String), StorageError> {
    let (ts, id_hex) = c.0.split_once('|').ok_or_else(|| {
        StorageError::invalid_input("notifications cursor must be `<timestamp>|<hex-id>`")
    })?;
    let _ = parse_ts(ts)?;
    Ok((ts.to_string(), id_hex.to_string()))
}

fn finalise_page(mut rows: Vec<Row>, limit: u32) -> Result<PageSlice<Notification>, StorageError> {
    let limit_usize = limit as usize;
    let has_more = rows.len() > limit_usize;
    if has_more {
        rows.truncate(limit_usize);
    }
    let next = if has_more {
        rows.last().map(|last| encode_cursor(&last.5, &last.0))
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(row_to_notification)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PageSlice { items, next })
}
