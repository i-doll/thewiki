//! libsql [`RoleRepository`](crate::repo::RoleRepository) impl.

use libsql::{Connection, Value, params};
use thewiki_core::{Role, RoleId, RoleName, UserId};

use crate::error::StorageError;
use crate::libsql::codec::{
    into_db, map_unique_violation, permissions_to_i64, role_from_libsql_row, uuid_bytes,
};
use crate::repo::RoleRepository;

/// libsql-backed role repository.
pub struct LibsqlRoleRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LibsqlRoleRepository<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl RoleRepository for LibsqlRoleRepository<'_> {
    async fn create(&self, role: &Role) -> Result<(), StorageError> {
        let id = uuid_bytes(role.id.into_uuid());
        let permissions = permissions_to_i64(role.permissions);

        let result = self
            .conn
            .execute(
                "INSERT INTO roles (id, name, display_name, permissions) VALUES (?1, ?2, ?3, ?4)",
                params![
                    Value::Blob(id.to_vec()),
                    role.name.as_str().to_owned(),
                    role.display_name.clone(),
                    permissions,
                ],
            )
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(err, "role name already in use")),
        }
    }

    async fn get_by_id(&self, id: RoleId) -> Result<Role, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, name, display_name, permissions FROM roles WHERE id = ?1",
                    params![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        match row {
            Some(row) => role_from_libsql_row(&row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_name(&self, name: &RoleName) -> Result<Role, StorageError> {
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, name, display_name, permissions FROM roles WHERE name = ?1",
                    params![name.as_str().to_owned()],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        match row {
            Some(row) => role_from_libsql_row(&row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn list(&self) -> Result<Vec<Role>, StorageError> {
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, name, display_name, permissions FROM roles ORDER BY name ASC",
                    (),
                )
                .await,
        )?;
        let mut out = Vec::new();
        while let Some(row) = into_db(rows.next().await)? {
            out.push(role_from_libsql_row(&row)?);
        }
        Ok(out)
    }

    async fn assign_to_user(&self, user_id: UserId, role_id: RoleId) -> Result<(), StorageError> {
        let user = uuid_bytes(user_id.into_uuid());
        let role = uuid_bytes(role_id.into_uuid());
        // ON CONFLICT: idempotent — assigning a role twice is a no-op.
        into_db(
            self.conn
                .execute(
                    "INSERT INTO user_roles (user_id, role_id) VALUES (?1, ?2)
                     ON CONFLICT (user_id, role_id) DO NOTHING",
                    params![Value::Blob(user.to_vec()), Value::Blob(role.to_vec())],
                )
                .await,
        )?;
        Ok(())
    }

    async fn revoke_from_user(&self, user_id: UserId, role_id: RoleId) -> Result<(), StorageError> {
        let user = uuid_bytes(user_id.into_uuid());
        let role = uuid_bytes(role_id.into_uuid());
        into_db(
            self.conn
                .execute(
                    "DELETE FROM user_roles WHERE user_id = ?1 AND role_id = ?2",
                    params![Value::Blob(user.to_vec()), Value::Blob(role.to_vec())],
                )
                .await,
        )?;
        Ok(())
    }

    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<Role>, StorageError> {
        let user = uuid_bytes(user_id.into_uuid());
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT r.id, r.name, r.display_name, r.permissions
                     FROM roles r
                     JOIN user_roles ur ON ur.role_id = r.id
                     WHERE ur.user_id = ?1
                     ORDER BY r.name ASC",
                    params![Value::Blob(user.to_vec())],
                )
                .await,
        )?;
        let mut out = Vec::new();
        while let Some(row) = into_db(rows.next().await)? {
            out.push(role_from_libsql_row(&row)?);
        }
        Ok(out)
    }

    async fn update(&self, role: &Role) -> Result<(), StorageError> {
        let id = uuid_bytes(role.id.into_uuid());
        let permissions = permissions_to_i64(role.permissions);
        let rows_affected = into_db(
            self.conn
                .execute(
                    "UPDATE roles SET display_name = ?1, permissions = ?2 WHERE id = ?3",
                    params![role.display_name.clone(), permissions, Value::Blob(id.to_vec())],
                )
                .await,
        )?;
        if rows_affected == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn delete(&self, id: RoleId) -> Result<(), StorageError> {
        // user_roles.role_id is ON DELETE CASCADE — see the matching SQLite
        // impl for the rationale of refusing here when the role is still
        // assigned to one or more users.
        let assigned = self.count_users(id).await?;
        if assigned > 0 {
            return Err(StorageError::conflict(
                "role is still assigned to one or more users",
            ));
        }
        let id_bytes = uuid_bytes(id.into_uuid());
        let rows_affected = into_db(
            self.conn
                .execute(
                    "DELETE FROM roles WHERE id = ?1",
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

    async fn count_users(&self, id: RoleId) -> Result<u64, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT COUNT(*) FROM user_roles WHERE role_id = ?1",
                    params![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?
            .ok_or_else(|| StorageError::invalid_input("count returned no rows"))?;
        let val = row
            .get_value(0)
            .map_err(|err| StorageError::InvalidInput(format!("count: {err}")))?;
        let count = match val {
            Value::Integer(i) if i >= 0 => i as u64,
            Value::Integer(_) => 0,
            other => {
                return Err(StorageError::InvalidInput(format!(
                    "count is not INTEGER: {other:?}"
                )))
            }
        };
        Ok(count)
    }
}
