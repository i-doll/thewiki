//! libsql [`WatchRepository`](crate::repo::WatchRepository) impl (#46).
//!
//! Mirror of the SQLite adapter — schema is portable between the two engines,
//! only the driver call site differs.

use libsql::{Connection, Value};
use thewiki_core::{NamespaceId, PageId, UserId};
use time::OffsetDateTime;

use crate::codec::parse_protection_level;
use crate::error::StorageError;
use crate::libsql::codec::{decode_uuid, format_ts, into_db, parse_ts, uuid_bytes};
use crate::repo::{WatchRepository, WatchedPage, clamp_limit};

/// libsql-backed watchlist repository.
pub struct LibsqlWatchRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LibsqlWatchRepository<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl WatchRepository for LibsqlWatchRepository<'_> {
    async fn watch(&self, user_id: UserId, page_id: PageId) -> Result<(), StorageError> {
        let user_bytes = uuid_bytes(user_id.into_uuid());
        let page_bytes = uuid_bytes(page_id.into_uuid());
        let created_at = format_ts(OffsetDateTime::now_utc())?;
        into_db(
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO watch (user_id, page_id, created_at)
                     VALUES (?1, ?2, ?3)",
                    vec![
                        Value::Blob(user_bytes.to_vec()),
                        Value::Blob(page_bytes.to_vec()),
                        Value::Text(created_at),
                    ],
                )
                .await,
        )?;
        Ok(())
    }

    async fn unwatch(&self, user_id: UserId, page_id: PageId) -> Result<(), StorageError> {
        let user_bytes = uuid_bytes(user_id.into_uuid());
        let page_bytes = uuid_bytes(page_id.into_uuid());
        into_db(
            self.conn
                .execute(
                    "DELETE FROM watch WHERE user_id = ?1 AND page_id = ?2",
                    vec![
                        Value::Blob(user_bytes.to_vec()),
                        Value::Blob(page_bytes.to_vec()),
                    ],
                )
                .await,
        )?;
        Ok(())
    }

    async fn is_watched(&self, user_id: UserId, page_id: PageId) -> Result<bool, StorageError> {
        let user_bytes = uuid_bytes(user_id.into_uuid());
        let page_bytes = uuid_bytes(page_id.into_uuid());
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT 1 FROM watch WHERE user_id = ?1 AND page_id = ?2 LIMIT 1",
                    vec![
                        Value::Blob(user_bytes.to_vec()),
                        Value::Blob(page_bytes.to_vec()),
                    ],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        Ok(row.is_some())
    }

    async fn list_for_user(
        &self,
        user_id: UserId,
        limit: u32,
    ) -> Result<Vec<WatchedPage>, StorageError> {
        let limit = clamp_limit(limit);
        let take = i64::from(limit);
        let user_bytes = uuid_bytes(user_id.into_uuid());

        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT pages.id, pages.namespace_id, namespaces.slug,
                            pages.slug, pages.title, pages.protection_level,
                            watch.created_at, pages.updated_at
                     FROM watch
                     JOIN pages      ON pages.id      = watch.page_id
                     JOIN namespaces ON namespaces.id = pages.namespace_id
                     WHERE watch.user_id = ?1
                     ORDER BY watch.created_at DESC, watch.page_id DESC
                     LIMIT ?2",
                    vec![Value::Blob(user_bytes.to_vec()), Value::Integer(take)],
                )
                .await,
        )?;

        let mut out = Vec::with_capacity(limit as usize);
        while let Some(row) = into_db(rows.next().await)? {
            let id: Vec<u8> = into_db(row.get::<Vec<u8>>(0))?;
            let ns_id: Vec<u8> = into_db(row.get::<Vec<u8>>(1))?;
            let ns_slug: String = into_db(row.get::<String>(2))?;
            let page_slug: String = into_db(row.get::<String>(3))?;
            let title: String = into_db(row.get::<String>(4))?;
            let prot: String = into_db(row.get::<String>(5))?;
            let watched_at: String = into_db(row.get::<String>(6))?;
            let updated_at: String = into_db(row.get::<String>(7))?;
            out.push(WatchedPage {
                page_id: PageId::from_uuid(decode_uuid(&id)?),
                namespace_id: NamespaceId::from_uuid(decode_uuid(&ns_id)?),
                namespace_slug: ns_slug,
                page_slug,
                page_title: title,
                protection_level: parse_protection_level(&prot)?,
                watched_at: parse_ts(&watched_at)?,
                updated_at: parse_ts(&updated_at)?,
            });
        }
        Ok(out)
    }
}
