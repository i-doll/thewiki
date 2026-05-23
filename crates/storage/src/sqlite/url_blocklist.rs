//! SQLite [`UrlBlocklistRepository`](crate::repo::UrlBlocklistRepository) impl (#42).

use sqlx::SqlitePool;
use thewiki_core::UserId;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::StorageError;
use crate::repo::{NewUrlBlocklistEntry, UrlBlocklistEntry, UrlBlocklistRepository};
use crate::sqlite::codec::{decode_uuid, format_ts, map_unique_violation, parse_ts, uuid_bytes};

type Row = (Vec<u8>, String, String, Vec<u8>, String);

fn row_to_entry(row: Row) -> Result<UrlBlocklistEntry, StorageError> {
    let (id, pattern, reason, created_by, created_at) = row;
    Ok(UrlBlocklistEntry {
        id: decode_uuid(&id)?,
        pattern,
        reason,
        created_by: UserId::from_uuid(decode_uuid(&created_by)?),
        created_at: parse_ts(&created_at)?,
    })
}

/// SQLite-backed URL blocklist repository.
pub struct SqliteUrlBlocklistRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteUrlBlocklistRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl UrlBlocklistRepository for SqliteUrlBlocklistRepository<'_> {
    async fn create(
        &self,
        entry: NewUrlBlocklistEntry,
    ) -> Result<UrlBlocklistEntry, StorageError> {
        let id = Uuid::now_v7();
        let id_bytes = uuid_bytes(id);
        let created_by_bytes = uuid_bytes(entry.created_by.into_uuid());
        let created_at = OffsetDateTime::now_utc();
        let created_at_str = format_ts(created_at)?;

        sqlx::query(
            "INSERT INTO url_blocklist (id, pattern, reason, created_by, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(id_bytes.as_slice())
        .bind(&entry.pattern)
        .bind(&entry.reason)
        .bind(created_by_bytes.as_slice())
        .bind(&created_at_str)
        .execute(self.pool)
        .await
        .map_err(|e| map_unique_violation(e, "url_blocklist.pattern already exists"))?;

        Ok(UrlBlocklistEntry {
            id,
            pattern: entry.pattern,
            reason: entry.reason,
            created_by: entry.created_by,
            created_at,
        })
    }

    async fn list_all(&self) -> Result<Vec<UrlBlocklistEntry>, StorageError> {
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT id, pattern, reason, created_by, created_at \
             FROM url_blocklist \
             ORDER BY created_at DESC, id DESC",
        )
        .fetch_all(self.pool)
        .await?;
        rows.into_iter().map(row_to_entry).collect()
    }

    async fn get_by_id(&self, id: Uuid) -> Result<UrlBlocklistEntry, StorageError> {
        let id_bytes = uuid_bytes(id);
        let row: Option<Row> = sqlx::query_as(
            "SELECT id, pattern, reason, created_by, created_at \
             FROM url_blocklist \
             WHERE id = ?1",
        )
        .bind(id_bytes.as_slice())
        .fetch_optional(self.pool)
        .await?;
        row.ok_or(StorageError::NotFound).and_then(row_to_entry)
    }

    async fn delete(&self, id: Uuid) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id);
        let result = sqlx::query("DELETE FROM url_blocklist WHERE id = ?1")
            .bind(id_bytes.as_slice())
            .execute(self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }
}
