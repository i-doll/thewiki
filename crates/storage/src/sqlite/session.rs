//! SQLite [`SessionRepository`](crate::repo::SessionRepository) impl.

use std::time::Duration;

use sqlx::SqlitePool;
use thewiki_core::{Session, SessionId, UserId};
use time::OffsetDateTime;

use crate::error::StorageError;
use crate::repo::SessionRepository;
use crate::sqlite::codec::{format_ts, session_from_row, uuid_bytes};

type SessionRow = (
    Vec<u8>,        // id
    Vec<u8>,        // user_id
    String,         // created_at
    String,         // expires_at
    String,         // last_seen_at
    Option<String>, // user_agent
    Option<String>, // ip_address
);

fn row_to_session(row: SessionRow) -> Result<Session, StorageError> {
    let (id, user_id, created_at, expires_at, last_seen_at, ua, ip) = row;
    session_from_row(id, user_id, created_at, expires_at, last_seen_at, ua, ip)
}

/// SQLite-backed session repository.
pub struct SqliteSessionRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteSessionRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl SessionRepository for SqliteSessionRepository<'_> {
    async fn create(
        &self,
        user_id: UserId,
        ttl: Duration,
        user_agent: Option<&str>,
        ip_address: Option<&str>,
    ) -> Result<Session, StorageError> {
        let id = SessionId::new();
        let now = OffsetDateTime::now_utc();
        // `time::Duration::try_from` clamps on overflow; for a session TTL it
        // would have to be > 292 billion years to overflow, so a saturating
        // fallback is safe here.
        let ttl_t = time::Duration::try_from(ttl).unwrap_or(time::Duration::MAX);
        let expires = now.saturating_add(ttl_t);

        let id_bytes = uuid_bytes(id.into_uuid());
        let user_bytes = uuid_bytes(user_id.into_uuid());
        let created_str = format_ts(now)?;
        let expires_str = format_ts(expires)?;
        let last_seen_str = created_str.clone();

        sqlx::query(
            "INSERT INTO sessions
                (id, user_id, created_at, expires_at, last_seen_at, user_agent, ip_address)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(id_bytes.as_slice())
        .bind(user_bytes.as_slice())
        .bind(&created_str)
        .bind(&expires_str)
        .bind(&last_seen_str)
        .bind(user_agent)
        .bind(ip_address)
        .execute(self.pool)
        .await?;

        Ok(Session {
            id,
            user_id,
            created_at: now,
            expires_at: expires,
            last_seen_at: now,
            user_agent: user_agent.map(str::to_owned),
            ip_address: ip_address.map(str::to_owned),
        })
    }

    async fn get_by_id(&self, id: SessionId) -> Result<Session, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let row: Option<SessionRow> = sqlx::query_as(
            "SELECT id, user_id, created_at, expires_at, last_seen_at, user_agent, ip_address
             FROM sessions WHERE id = ?1",
        )
        .bind(id_bytes.as_slice())
        .fetch_optional(self.pool)
        .await?;

        let Some(row) = row else {
            return Err(StorageError::NotFound);
        };
        let session = row_to_session(row)?;
        // Treat expired sessions as not found so callers don't have to repeat
        // the TTL check at every site. We *deliberately* parse the column
        // rather than filtering in SQL — RFC3339 strings sort lexicographically
        // for the same offset, but we don't want to bake that assumption in.
        let now = OffsetDateTime::now_utc();
        if session.is_expired_at(now) {
            return Err(StorageError::NotFound);
        }
        Ok(session)
    }

    async fn touch(&self, id: SessionId) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let now = format_ts(OffsetDateTime::now_utc())?;
        let out = sqlx::query("UPDATE sessions SET last_seen_at = ?1 WHERE id = ?2")
            .bind(&now)
            .bind(id_bytes.as_slice())
            .execute(self.pool)
            .await?;
        if out.rows_affected() == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn delete(&self, id: SessionId) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let out = sqlx::query("DELETE FROM sessions WHERE id = ?1")
            .bind(id_bytes.as_slice())
            .execute(self.pool)
            .await?;
        if out.rows_affected() == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn delete_for_user(&self, user_id: UserId) -> Result<u64, StorageError> {
        let user_bytes = uuid_bytes(user_id.into_uuid());
        let out = sqlx::query("DELETE FROM sessions WHERE user_id = ?1")
            .bind(user_bytes.as_slice())
            .execute(self.pool)
            .await?;
        Ok(out.rows_affected())
    }

    async fn prune_expired(&self) -> Result<u64, StorageError> {
        let now = format_ts(OffsetDateTime::now_utc())?;
        let out = sqlx::query("DELETE FROM sessions WHERE expires_at <= ?1")
            .bind(&now)
            .execute(self.pool)
            .await?;
        Ok(out.rows_affected())
    }
}
