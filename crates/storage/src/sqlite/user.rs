//! SQLite [`UserRepository`](crate::repo::UserRepository) impl.

use sqlx::SqlitePool;
use thewiki_core::{User, UserId, Username};

use crate::error::StorageError;
use crate::repo::{Cursor, PageSlice, UserListFilter, UserRepository};
use crate::sqlite::codec::{format_ts, map_unique_violation, user_from_row, uuid_bytes};

type UserRow = (
    Vec<u8>,        // id
    String,         // username
    Option<String>, // email
    Option<String>, // display_name
    String,         // created_at
    Option<String>, // last_login_at
);

fn row_to_user(row: UserRow) -> Result<User, StorageError> {
    let (id, username, email, display_name, created_at, last_login_at) = row;
    user_from_row(id, username, email, display_name, created_at, last_login_at)
}

/// SQLite-backed user repository.
pub struct SqliteUserRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteUserRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl UserRepository for SqliteUserRepository<'_> {
    async fn create(&self, user: &User, password_hash: Option<&str>) -> Result<(), StorageError> {
        let id = uuid_bytes(user.id.into_uuid());
        let created_at = format_ts(user.created_at)?;
        let last_login_at = user.last_login_at.map(format_ts).transpose()?;

        let result = sqlx::query(
            "INSERT INTO users
                (id, username, email, display_name, password_hash, created_at, last_login_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(id.as_slice())
        .bind(user.username.as_str())
        .bind(user.email.as_ref().map(|e| e.as_str()))
        .bind(user.display_name.as_deref())
        .bind(password_hash)
        .bind(&created_at)
        .bind(&last_login_at)
        .execute(self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(err, "username already taken")),
        }
    }

    async fn get_by_id(&self, id: UserId) -> Result<User, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let row: Option<UserRow> = sqlx::query_as(
            "SELECT id, username, email, display_name, created_at, last_login_at
             FROM users WHERE id = ?1",
        )
        .bind(id_bytes.as_slice())
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some(row) => row_to_user(row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_username(&self, username: &Username) -> Result<User, StorageError> {
        let row: Option<UserRow> = sqlx::query_as(
            "SELECT id, username, email, display_name, created_at, last_login_at
             FROM users WHERE username = ?1",
        )
        .bind(username.as_str())
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some(row) => row_to_user(row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn update(&self, user: &User) -> Result<(), StorageError> {
        let id = uuid_bytes(user.id.into_uuid());
        let last_login_at = user.last_login_at.map(format_ts).transpose()?;

        let out = sqlx::query(
            "UPDATE users
             SET email = ?1,
                 display_name = ?2,
                 last_login_at = ?3
             WHERE id = ?4",
        )
        .bind(user.email.as_ref().map(|e| e.as_str()))
        .bind(user.display_name.as_deref())
        .bind(&last_login_at)
        .bind(id.as_slice())
        .execute(self.pool)
        .await?;

        if out.rows_affected() == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn delete(&self, id: UserId) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let result = sqlx::query("DELETE FROM users WHERE id = ?1")
            .bind(id_bytes.as_slice())
            .execute(self.pool)
            .await;

        match result {
            Ok(out) => {
                if out.rows_affected() == 0 {
                    Err(StorageError::NotFound)
                } else {
                    Ok(())
                }
            }
            Err(err) => {
                // FK violation from `revisions.author_id ON DELETE RESTRICT`
                // is the realistic failure mode here.
                if let Some(db_err) = err.as_database_error()
                    && matches!(db_err.code().as_deref(), Some("1811" | "787"))
                {
                    Err(StorageError::conflict(
                        "user has revisions and cannot be deleted",
                    ))
                } else {
                    Err(StorageError::Database(err))
                }
            }
        }
    }

    async fn list(
        &self,
        filter: UserListFilter,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<PageSlice<User>, StorageError> {
        // Clamp the limit to the shared upper bound so a malicious caller
        // can't trigger an unbounded scan. `+1` is the peek-next trick —
        // ask for one extra row and use the overshoot to decide whether
        // another page exists.
        let raw_limit = if limit == 0 {
            crate::repo::DEFAULT_PAGE_SIZE
        } else {
            limit.min(crate::repo::MAX_PAGE_SIZE)
        };
        let scan_limit = raw_limit as i64 + 1;

        // Cursor encoding: `<rfc3339_created_at>|<hex_id>`. Parse defensively.
        let cursor_parts = cursor
            .as_ref()
            .map(|c| parse_cursor(&c.0))
            .transpose()?
            .flatten();

        // Build the statement dynamically because sqlx::query lacks dynamic
        // binder ergonomics. Every literal parameter is still bound via `?`
        // so the SQL stays parameterised end-to-end.
        let mut sql = String::from(
            "SELECT u.id, u.username, u.email, u.display_name, u.created_at, u.last_login_at \
             FROM users u",
        );
        if filter.role_id.is_some() {
            sql.push_str(" INNER JOIN user_roles ur ON ur.user_id = u.id");
        }
        sql.push_str(" WHERE 1=1");
        if filter.role_id.is_some() {
            sql.push_str(" AND ur.role_id = ?");
        }
        let needle = filter
            .search
            .as_ref()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        if needle.is_some() {
            sql.push_str(
                " AND (lower(u.username) LIKE ? \
                   OR (u.email IS NOT NULL AND lower(u.email) LIKE ?))",
            );
        }
        if cursor_parts.is_some() {
            sql.push_str(" AND (u.created_at, u.id) > (?, ?)");
        }
        sql.push_str(" ORDER BY u.created_at ASC, u.id ASC LIMIT ?");

        let mut q = sqlx::query_as::<sqlx::Sqlite, UserRow>(&sql);
        if let Some(role_id) = filter.role_id {
            let role_bytes = uuid_bytes(role_id.into_uuid());
            q = q.bind(role_bytes.to_vec());
        }
        if let Some(ref n) = needle {
            let like = format!("%{n}%");
            q = q.bind(like.clone()).bind(like);
        }
        if let Some((ref ts, ref id_bytes)) = cursor_parts {
            q = q.bind(ts.clone()).bind(id_bytes.clone());
        }
        q = q.bind(scan_limit);

        let rows: Vec<UserRow> = q.fetch_all(self.pool).await?;
        let overshoot = rows.len() > raw_limit as usize;

        // Cap to the requested batch and convert to domain users. The
        // cursor seed is the last *included* row's (created_at, id) — the
        // next page starts strictly after it.
        let take = rows.into_iter().take(raw_limit as usize);
        let mut items = Vec::with_capacity(raw_limit as usize);
        let mut next_seed: Option<(String, Vec<u8>)> = None;
        for row in take {
            next_seed = Some((row.4.clone(), row.0.clone()));
            items.push(row_to_user(row)?);
        }

        let next = if overshoot {
            next_seed.map(|(ts, id_bytes)| Cursor(encode_cursor(&ts, &id_bytes)))
        } else {
            None
        };
        Ok(PageSlice { items, next })
    }
}

/// Encode the `(created_at, id)` pair as the opaque cursor token.
///
/// Format: `<rfc3339>|<hex(id)>` — both halves are URL-safe ASCII so the
/// token survives query-string round-trips.
fn encode_cursor(ts: &str, id: &[u8]) -> String {
    let mut hex = String::with_capacity(id.len() * 2);
    for byte in id {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    format!("{ts}|{hex}")
}

/// Parse the cursor produced by [`encode_cursor`]. Returns `None` for an
/// empty cursor (treated as "start").
fn parse_cursor(raw: &str) -> Result<Option<(String, Vec<u8>)>, StorageError> {
    if raw.is_empty() {
        return Ok(None);
    }
    let (ts, hex) = raw
        .split_once('|')
        .ok_or_else(|| StorageError::invalid_input("malformed user cursor"))?;
    if hex.len() % 2 != 0 {
        return Err(StorageError::invalid_input("malformed user cursor (hex)"));
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(2) {
        let s = std::str::from_utf8(chunk)
            .map_err(|_| StorageError::invalid_input("malformed user cursor (utf-8)"))?;
        let b = u8::from_str_radix(s, 16)
            .map_err(|_| StorageError::invalid_input("malformed user cursor (hex byte)"))?;
        bytes.push(b);
    }
    Ok(Some((ts.to_owned(), bytes)))
}
