//! Postgres [`UserRepository`](crate::repo::UserRepository) impl.

use sqlx::PgPool;
use thewiki_core::{User, UserId, Username};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::StorageError;
use crate::postgres::codec::{is_fk_violation, map_unique_violation, user_from_row};
use crate::repo::UserRepository;

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
}
