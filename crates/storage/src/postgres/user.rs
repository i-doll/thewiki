//! Postgres [`UserRepository`](crate::repo::UserRepository) impl.

use sqlx::PgPool;
use thewiki_core::{User, UserId, Username};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::StorageError;
use crate::postgres::codec::{is_fk_violation, map_unique_violation, user_from_row};
use crate::repo::{Cursor, PageSlice, UserListFilter, UserRepository};

type UserRow = (
    Uuid,                   // id
    String,                 // username
    Option<String>,         // email
    Option<String>,         // display_name
    OffsetDateTime,         // created_at
    Option<OffsetDateTime>, // last_login_at
);

fn row_to_user(row: UserRow) -> Result<User, StorageError> {
    let (id, username, email, display_name, created_at, last_login_at) = row;
    user_from_row(id, username, email, display_name, created_at, last_login_at)
}

/// Postgres-backed user repository.
pub struct PostgresUserRepository<'a> {
    pool: &'a PgPool,
}

impl<'a> PostgresUserRepository<'a> {
    pub(super) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }
}

impl UserRepository for PostgresUserRepository<'_> {
    async fn create(&self, user: &User, password_hash: Option<&str>) -> Result<(), StorageError> {
        let result = sqlx::query(
            "INSERT INTO users
                (id, username, email, display_name, password_hash, created_at, last_login_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(user.id.into_uuid())
        .bind(user.username.as_str())
        .bind(user.email.as_ref().map(|e| e.as_str()))
        .bind(user.display_name.as_deref())
        .bind(password_hash)
        .bind(user.created_at)
        .bind(user.last_login_at)
        .execute(self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(err, "username already taken")),
        }
    }

    async fn get_by_id(&self, id: UserId) -> Result<User, StorageError> {
        let row: Option<UserRow> = sqlx::query_as(
            "SELECT id, username, email, display_name, created_at, last_login_at
             FROM users WHERE id = $1",
        )
        .bind(id.into_uuid())
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
             FROM users WHERE username = $1",
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
        let out = sqlx::query(
            "UPDATE users
             SET email = $1,
                 display_name = $2,
                 last_login_at = $3
             WHERE id = $4",
        )
        .bind(user.email.as_ref().map(|e| e.as_str()))
        .bind(user.display_name.as_deref())
        .bind(user.last_login_at)
        .bind(user.id.into_uuid())
        .execute(self.pool)
        .await?;

        if out.rows_affected() == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn delete(&self, id: UserId) -> Result<(), StorageError> {
        let result = sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id.into_uuid())
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
                // is the realistic failure mode here. Postgres SQLSTATE 23503
                // signals foreign_key_violation.
                if is_fk_violation(&err) {
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
        let raw_limit = if limit == 0 {
            crate::repo::DEFAULT_PAGE_SIZE
        } else {
            limit.min(crate::repo::MAX_PAGE_SIZE)
        };
        let scan_limit = raw_limit as i64 + 1;

        let cursor_parts = cursor
            .as_ref()
            .map(|c| parse_cursor(&c.0))
            .transpose()?
            .flatten();

        // Build the statement dynamically — sqlx::query lacks dynamic-bind
        // ergonomics, so we splice `$N` placeholders and bind in order.
        let mut next_placeholder: usize = 1;
        let mut take_ph = || {
            let p = next_placeholder;
            next_placeholder += 1;
            format!("${p}")
        };
        let mut sql = String::from(
            "SELECT u.id, u.username, u.email, u.display_name, u.created_at, u.last_login_at \
             FROM users u",
        );
        if filter.role_id.is_some() {
            sql.push_str(" INNER JOIN user_roles ur ON ur.user_id = u.id");
        }
        sql.push_str(" WHERE TRUE");
        let role_ph = if filter.role_id.is_some() {
            let p = take_ph();
            sql.push_str(&format!(" AND ur.role_id = {p}"));
            Some(p)
        } else {
            None
        };
        let _ = role_ph; // captured implicitly by bind ordering below
        let needle = filter
            .search
            .as_ref()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        if needle.is_some() {
            let p1 = take_ph();
            let p2 = take_ph();
            sql.push_str(&format!(
                " AND (lower(u.username) LIKE {p1} OR (u.email IS NOT NULL AND lower(u.email) LIKE {p2}))"
            ));
        }
        if cursor_parts.is_some() {
            let p1 = take_ph();
            let p2 = take_ph();
            sql.push_str(&format!(" AND (u.created_at, u.id) > ({p1}, {p2})"));
        }
        let lim_p = take_ph();
        sql.push_str(&format!(
            " ORDER BY u.created_at ASC, u.id ASC LIMIT {lim_p}"
        ));

        let mut q = sqlx::query_as::<_, UserRow>(&sql);
        if let Some(role_id) = filter.role_id {
            q = q.bind(role_id.into_uuid());
        }
        if let Some(n) = needle.as_ref() {
            let like = format!("%{n}%");
            q = q.bind(like.clone()).bind(like);
        }
        if let Some((ts, id)) = cursor_parts.as_ref() {
            q = q.bind(*ts).bind(*id);
        }
        q = q.bind(scan_limit);

        let rows: Vec<UserRow> = q.fetch_all(self.pool).await?;
        let overshoot = rows.len() > raw_limit as usize;

        let take = rows.into_iter().take(raw_limit as usize);
        let mut items = Vec::with_capacity(raw_limit as usize);
        let mut next_seed: Option<(OffsetDateTime, Uuid)> = None;
        for row in take {
            next_seed = Some((row.4, row.0));
            items.push(row_to_user(row)?);
        }

        let next = if overshoot {
            next_seed.map(|(ts, id)| Cursor(encode_cursor(ts, id)))
        } else {
            None
        };
        Ok(PageSlice { items, next })
    }
}

/// Encode the `(created_at, id)` pair as the opaque cursor token.
///
/// Format: `<rfc3339>|<uuid-hyphenated>` — both halves are URL-safe ASCII.
fn encode_cursor(ts: OffsetDateTime, id: Uuid) -> String {
    use time::format_description::well_known::Rfc3339;
    // `format` on a UTC offset is infallible for OffsetDateTime values
    // we constructed ourselves; fall back to the Display impl so we don't
    // panic on a hypothetical bad format-description error.
    let ts_str = ts.format(&Rfc3339).unwrap_or_else(|_| ts.to_string());
    format!("{ts_str}|{}", id.as_hyphenated())
}

fn parse_cursor(raw: &str) -> Result<Option<(OffsetDateTime, Uuid)>, StorageError> {
    use time::format_description::well_known::Rfc3339;
    if raw.is_empty() {
        return Ok(None);
    }
    let (ts_str, id_str) = raw
        .split_once('|')
        .ok_or_else(|| StorageError::invalid_input("malformed user cursor"))?;
    let ts = OffsetDateTime::parse(ts_str, &Rfc3339)
        .map_err(|_| StorageError::invalid_input("malformed user cursor (timestamp)"))?;
    let id = Uuid::parse_str(id_str)
        .map_err(|_| StorageError::invalid_input("malformed user cursor (uuid)"))?;
    Ok(Some((ts, id)))
}
