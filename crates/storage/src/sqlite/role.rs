//! SQLite [`RoleRepository`](crate::repo::RoleRepository) impl.

use sqlx::SqlitePool;
use thewiki_core::{Role, RoleId, RoleName, UserId};

use crate::error::StorageError;
use crate::repo::RoleRepository;
use crate::sqlite::codec::{map_unique_violation, permissions_to_i64, role_from_row, uuid_bytes};

type RoleRow = (
    Vec<u8>, // id
    String,  // name
    String,  // display_name
    i64,     // permissions
);

fn row_to_role(row: RoleRow) -> Result<Role, StorageError> {
    let (id, name, display_name, permissions) = row;
    role_from_row(id, name, display_name, permissions)
}

/// SQLite-backed role repository.
pub struct SqliteRoleRepository<'a> {
    pool: &'a SqlitePool,
}

impl<'a> SqliteRoleRepository<'a> {
    pub(super) fn new(pool: &'a SqlitePool) -> Self {
        Self { pool }
    }
}

impl RoleRepository for SqliteRoleRepository<'_> {
    async fn create(&self, role: &Role) -> Result<(), StorageError> {
        let id = uuid_bytes(role.id.into_uuid());
        let permissions = permissions_to_i64(role.permissions);

        let result = sqlx::query(
            "INSERT INTO roles (id, name, display_name, permissions) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(id.as_slice())
        .bind(role.name.as_str())
        .bind(&role.display_name)
        .bind(permissions)
        .execute(self.pool)
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(err, "role name already in use")),
        }
    }

    async fn get_by_id(&self, id: RoleId) -> Result<Role, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let row: Option<RoleRow> =
            sqlx::query_as("SELECT id, name, display_name, permissions FROM roles WHERE id = ?1")
                .bind(id_bytes.as_slice())
                .fetch_optional(self.pool)
                .await?;

        match row {
            Some(row) => row_to_role(row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_name(&self, name: &RoleName) -> Result<Role, StorageError> {
        let row: Option<RoleRow> =
            sqlx::query_as("SELECT id, name, display_name, permissions FROM roles WHERE name = ?1")
                .bind(name.as_str())
                .fetch_optional(self.pool)
                .await?;

        match row {
            Some(row) => row_to_role(row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn list(&self) -> Result<Vec<Role>, StorageError> {
        let rows: Vec<RoleRow> = sqlx::query_as(
            "SELECT id, name, display_name, permissions FROM roles ORDER BY name ASC",
        )
        .fetch_all(self.pool)
        .await?;

        rows.into_iter().map(row_to_role).collect()
    }

    async fn assign_to_user(&self, user_id: UserId, role_id: RoleId) -> Result<(), StorageError> {
        let user = uuid_bytes(user_id.into_uuid());
        let role = uuid_bytes(role_id.into_uuid());
        // ON CONFLICT: idempotent — assigning a role twice is a no-op.
        sqlx::query(
            "INSERT INTO user_roles (user_id, role_id) VALUES (?1, ?2)
             ON CONFLICT (user_id, role_id) DO NOTHING",
        )
        .bind(user.as_slice())
        .bind(role.as_slice())
        .execute(self.pool)
        .await?;
        Ok(())
    }

    async fn revoke_from_user(&self, user_id: UserId, role_id: RoleId) -> Result<(), StorageError> {
        let user = uuid_bytes(user_id.into_uuid());
        let role = uuid_bytes(role_id.into_uuid());
        sqlx::query("DELETE FROM user_roles WHERE user_id = ?1 AND role_id = ?2")
            .bind(user.as_slice())
            .bind(role.as_slice())
            .execute(self.pool)
            .await?;
        Ok(())
    }

    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<Role>, StorageError> {
        let user = uuid_bytes(user_id.into_uuid());
        let rows: Vec<RoleRow> = sqlx::query_as(
            "SELECT r.id, r.name, r.display_name, r.permissions
             FROM roles r
             JOIN user_roles ur ON ur.role_id = r.id
             WHERE ur.user_id = ?1
             ORDER BY r.name ASC",
        )
        .bind(user.as_slice())
        .fetch_all(self.pool)
        .await?;

        rows.into_iter().map(row_to_role).collect()
    }
}
