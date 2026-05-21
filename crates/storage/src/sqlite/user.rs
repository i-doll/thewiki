//! SQLite [`UserRepository`](crate::repo::UserRepository) impl.

use sqlx::SqlitePool;
use thewiki_core::{User, UserId, Username};

use crate::error::StorageError;
use crate::repo::UserRepository;
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
}
