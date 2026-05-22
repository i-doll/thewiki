//! libsql [`SessionRepository`](crate::repo::SessionRepository) impl.

use std::time::Duration;

use libsql::{Connection, Value, params};
use thewiki_core::{Session, SessionId, UserId};
use time::OffsetDateTime;

use crate::error::StorageError;
use crate::libsql::codec::{format_ts, into_db, opt_text, session_from_libsql_row, uuid_bytes};
use crate::repo::SessionRepository;

/// libsql-backed session repository.
pub struct LibsqlSessionRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LibsqlSessionRepository<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl SessionRepository for LibsqlSessionRepository<'_> {
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

        into_db(
            self.conn
                .execute(
                    "INSERT INTO sessions
                        (id, user_id, created_at, expires_at, last_seen_at, user_agent, ip_address)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        Value::Blob(id_bytes.to_vec()),
                        Value::Blob(user_bytes.to_vec()),
                        created_str,
                        expires_str,
                        last_seen_str,
                        opt_text(user_agent),
                        opt_text(ip_address),
                    ],
                )
                .await,
        )?;

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
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, user_id, created_at, expires_at, last_seen_at, user_agent, ip_address
                     FROM sessions WHERE id = ?1",
                    params![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        let Some(row) = row else {
            return Err(StorageError::NotFound);
        };
        let session = session_from_libsql_row(&row)?;
        // Match the SQLite adapter: treat expired sessions as not-found so the
        // call site doesn't have to repeat the TTL check.
        let now = OffsetDateTime::now_utc();
        if session.is_expired_at(now) {
            return Err(StorageError::NotFound);
        }
        Ok(session)
    }

    async fn touch(&self, id: SessionId) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let now = format_ts(OffsetDateTime::now_utc())?;
        let rows_affected = into_db(
            self.conn
                .execute(
                    "UPDATE sessions SET last_seen_at = ?1 WHERE id = ?2",
                    params![now, Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        if rows_affected == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn delete(&self, id: SessionId) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let rows_affected = into_db(
            self.conn
                .execute(
                    "DELETE FROM sessions WHERE id = ?1",
                    params![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        if rows_affected == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn delete_for_user(&self, user_id: UserId) -> Result<u64, StorageError> {
        let user_bytes = uuid_bytes(user_id.into_uuid());
        into_db(
            self.conn
                .execute(
                    "DELETE FROM sessions WHERE user_id = ?1",
                    params![Value::Blob(user_bytes.to_vec())],
                )
                .await,
        )
    }

    async fn prune_expired(&self) -> Result<u64, StorageError> {
        let now = format_ts(OffsetDateTime::now_utc())?;
        into_db(
            self.conn
                .execute("DELETE FROM sessions WHERE expires_at <= ?1", params![now])
                .await,
        )
    }
}
