//! Postgres [`SessionRepository`](crate::repo::SessionRepository) impl.

use std::time::Duration;

use sqlx::PgPool;
use thewiki_core::{Session, SessionId, UserId};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::StorageError;
use crate::postgres::codec::session_from_row;
use crate::repo::SessionRepository;

type SessionRow = (
    Uuid,           // id
    Uuid,           // user_id
    OffsetDateTime, // created_at
    OffsetDateTime, // expires_at
    OffsetDateTime, // last_seen_at
    Option<String>, // user_agent
    Option<String>, // ip_address
);

fn row_to_session(row: SessionRow) -> Result<Session, StorageError> {
    let (id, user_id, created_at, expires_at, last_seen_at, ua, ip) = row;
    session_from_row(id, user_id, created_at, expires_at, last_seen_at, ua, ip)
}

/// Postgres-backed session repository.
pub struct PostgresSessionRepository<'a> {
    pool: &'a PgPool,
}

impl<'a> PostgresSessionRepository<'a> {
    pub(super) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }
}

impl SessionRepository for PostgresSessionRepository<'_> {
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

        sqlx::query(
            "INSERT INTO sessions
                (id, user_id, created_at, expires_at, last_seen_at, user_agent, ip_address)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id.into_uuid())
        .bind(user_id.into_uuid())
        .bind(now)
        .bind(expires)
        .bind(now)
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
        let row: Option<SessionRow> = sqlx::query_as(
            "SELECT id, user_id, created_at, expires_at, last_seen_at, user_agent, ip_address
             FROM sessions WHERE id = $1",
        )
        .bind(id.into_uuid())
        .fetch_optional(self.pool)
        .await?;

        let Some(row) = row else {
            return Err(StorageError::NotFound);
        };
        let session = row_to_session(row)?;
        // Treat expired sessions as not found so callers don't have to repeat
        // the TTL check at every site.
        let now = OffsetDateTime::now_utc();
        if session.is_expired_at(now) {
            return Err(StorageError::NotFound);
        }
        Ok(session)
    }

    async fn touch(&self, id: SessionId) -> Result<(), StorageError> {
        let now = OffsetDateTime::now_utc();
        let out = sqlx::query("UPDATE sessions SET last_seen_at = $1 WHERE id = $2")
            .bind(now)
            .bind(id.into_uuid())
            .execute(self.pool)
            .await?;
        if out.rows_affected() == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn delete(&self, id: SessionId) -> Result<(), StorageError> {
        let out = sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(id.into_uuid())
            .execute(self.pool)
            .await?;
        if out.rows_affected() == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn delete_for_user(&self, user_id: UserId) -> Result<u64, StorageError> {
        let out = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(user_id.into_uuid())
            .execute(self.pool)
            .await?;
        Ok(out.rows_affected())
    }

    async fn prune_expired(&self) -> Result<u64, StorageError> {
        let now = OffsetDateTime::now_utc();
        let out = sqlx::query("DELETE FROM sessions WHERE expires_at <= $1")
            .bind(now)
            .execute(self.pool)
            .await?;
        Ok(out.rows_affected())
    }
}
