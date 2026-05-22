//! libsql [`UserRepository`](crate::repo::UserRepository) impl.

use libsql::{Connection, Value, params};
use thewiki_core::{User, UserId, Username};

use crate::error::StorageError;
use crate::libsql::codec::{
    format_ts, into_db, map_fk_restrict_violation, map_unique_violation, opt_text, opt_ts,
    user_from_libsql_row, uuid_bytes,
};
use crate::repo::UserRepository;

/// libsql-backed user repository.
pub struct LibsqlUserRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LibsqlUserRepository<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl UserRepository for LibsqlUserRepository<'_> {
    async fn create(&self, user: &User, password_hash: Option<&str>) -> Result<(), StorageError> {
        let id = uuid_bytes(user.id.into_uuid());
        let created_at = format_ts(user.created_at)?;
        let last_login_at = opt_ts(user.last_login_at)?;

        let result = self
            .conn
            .execute(
                "INSERT INTO users
                    (id, username, email, display_name, password_hash, created_at, last_login_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    Value::Blob(id.to_vec()),
                    user.username.as_str().to_owned(),
                    opt_text(user.email.as_ref().map(|e| e.as_str())),
                    opt_text(user.display_name.as_deref()),
                    opt_text(password_hash),
                    created_at,
                    last_login_at,
                ],
            )
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(err, "username already taken")),
        }
    }

    async fn get_by_id(&self, id: UserId) -> Result<User, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, username, email, display_name, created_at, last_login_at
                     FROM users WHERE id = ?1",
                    params![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        match row {
            Some(row) => user_from_libsql_row(&row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_username(&self, username: &Username) -> Result<User, StorageError> {
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, username, email, display_name, created_at, last_login_at
                     FROM users WHERE username = ?1",
                    params![username.as_str().to_owned()],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        match row {
            Some(row) => user_from_libsql_row(&row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn update(&self, user: &User) -> Result<(), StorageError> {
        let id = uuid_bytes(user.id.into_uuid());
        let last_login_at = opt_ts(user.last_login_at)?;

        let rows_affected = into_db(
            self.conn
                .execute(
                    "UPDATE users
                     SET email = ?1,
                         display_name = ?2,
                         last_login_at = ?3
                     WHERE id = ?4",
                    params![
                        opt_text(user.email.as_ref().map(|e| e.as_str())),
                        opt_text(user.display_name.as_deref()),
                        last_login_at,
                        Value::Blob(id.to_vec()),
                    ],
                )
                .await,
        )?;
        if rows_affected == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn delete(&self, id: UserId) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let result = self
            .conn
            .execute(
                "DELETE FROM users WHERE id = ?1",
                params![Value::Blob(id_bytes.to_vec())],
            )
            .await;

        match result {
            Ok(rows_affected) => {
                if rows_affected == 0 {
                    Err(StorageError::NotFound)
                } else {
                    Ok(())
                }
            }
            // `revisions.author_id ON DELETE RESTRICT` is the realistic failure
            // mode here — match it as a Conflict to mirror the SQLite adapter.
            Err(err) => Err(map_fk_restrict_violation(
                err,
                "user has revisions and cannot be deleted",
            )),
        }
    }
}
