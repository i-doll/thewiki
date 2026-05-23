//! libsql [`UserRepository`](crate::repo::UserRepository) impl.

use libsql::{Connection, Value, params};
use thewiki_core::{User, UserId, Username};

use crate::error::StorageError;
use crate::libsql::codec::{
    format_ts, into_db, map_fk_restrict_violation, map_unique_violation, opt_text, opt_ts,
    user_from_libsql_row, uuid_bytes,
};
use crate::repo::{Cursor, PageSlice, UserListFilter, UserRepository};

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

        let mut sql = String::from(
            "SELECT u.id, u.username, u.email, u.display_name, u.created_at, u.last_login_at \
             FROM users u",
        );
        if filter.role_id.is_some() {
            sql.push_str(" INNER JOIN user_roles ur ON ur.user_id = u.id");
        }
        sql.push_str(" WHERE 1=1");
        let mut params_vec: Vec<Value> = Vec::new();
        if let Some(role_id) = filter.role_id {
            sql.push_str(" AND ur.role_id = ?");
            params_vec.push(Value::Blob(uuid_bytes(role_id.into_uuid()).to_vec()));
        }
        let needle = filter
            .search
            .as_ref()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
        if let Some(n) = needle.as_ref() {
            sql.push_str(
                " AND (lower(u.username) LIKE ? \
                   OR (u.email IS NOT NULL AND lower(u.email) LIKE ?))",
            );
            let like = format!("%{n}%");
            params_vec.push(Value::Text(like.clone()));
            params_vec.push(Value::Text(like));
        }
        if let Some((ref ts, ref id_bytes)) = cursor_parts {
            sql.push_str(" AND (u.created_at, u.id) > (?, ?)");
            params_vec.push(Value::Text(ts.clone()));
            params_vec.push(Value::Blob(id_bytes.clone()));
        }
        sql.push_str(" ORDER BY u.created_at ASC, u.id ASC LIMIT ?");
        params_vec.push(Value::Integer(scan_limit));

        let mut rows = into_db(self.conn.query(&sql, params_vec).await)?;
        let mut collected: Vec<User> = Vec::new();
        let mut last_seed: Option<(String, Vec<u8>)> = None;
        let mut count: usize = 0;
        let mut overshoot = false;
        while let Some(row) = into_db(rows.next().await)? {
            if count == raw_limit as usize {
                overshoot = true;
                break;
            }
            // Extract created_at + id for cursor seeding before consuming.
            let id_val = row.get_value(0).map_err(|err| {
                StorageError::InvalidInput(format!("user row id: {err}"))
            })?;
            let created_val = row.get_value(4).map_err(|err| {
                StorageError::InvalidInput(format!("user row created_at: {err}"))
            })?;
            let id_bytes = match id_val {
                Value::Blob(bytes) => bytes,
                other => {
                    return Err(StorageError::InvalidInput(format!(
                        "user row id is not BLOB: {other:?}"
                    )))
                }
            };
            let created_text = match created_val {
                Value::Text(t) => t,
                other => {
                    return Err(StorageError::InvalidInput(format!(
                        "user row created_at is not TEXT: {other:?}"
                    )))
                }
            };
            last_seed = Some((created_text, id_bytes));
            let user = user_from_libsql_row(&row)?;
            collected.push(user);
            count += 1;
        }

        let next = if overshoot {
            last_seed.map(|(ts, id_bytes)| Cursor(encode_cursor(&ts, &id_bytes)))
        } else {
            None
        };
        Ok(PageSlice {
            items: collected,
            next,
        })
    }
}

fn encode_cursor(ts: &str, id: &[u8]) -> String {
    let mut hex = String::with_capacity(id.len() * 2);
    for byte in id {
        use std::fmt::Write;
        let _ = write!(hex, "{byte:02x}");
    }
    format!("{ts}|{hex}")
}

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
