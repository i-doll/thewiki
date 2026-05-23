//! Postgres [`RoleRepository`](crate::repo::RoleRepository) impl.

use std::collections::HashMap;

use sqlx::PgPool;
use thewiki_core::{Role, RoleId, RoleName, UserId};
use uuid::Uuid;

use crate::error::StorageError;
use crate::postgres::codec::{map_unique_violation, permissions_to_i64, role_from_row};
use crate::repo::RoleRepository;

type RoleRow = (
    Uuid,   // id
    String, // name
    String, // display_name
    i64,    // permissions
);

fn row_to_role(row: RoleRow) -> Result<Role, StorageError> {
    let (id, name, display_name, permissions) = row;
    role_from_row(id, name, display_name, permissions)
}

/// Postgres-backed role repository.
pub struct PostgresRoleRepository<'a> {
    pool: &'a PgPool,
}

impl<'a> PostgresRoleRepository<'a> {
    pub(super) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }
}

impl RoleRepository for PostgresRoleRepository<'_> {
    async fn create(&self, role: &Role) -> Result<(), StorageError> {
        let permissions = permissions_to_i64(role.permissions);

        let result = sqlx::query(
            "INSERT INTO roles (id, name, display_name, permissions) VALUES ($1, $2, $3, $4)",
        )
        .bind(role.id.into_uuid())
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
        let row: Option<RoleRow> =
            sqlx::query_as("SELECT id, name, display_name, permissions FROM roles WHERE id = $1")
                .bind(id.into_uuid())
                .fetch_optional(self.pool)
                .await?;

        match row {
            Some(row) => row_to_role(row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_name(&self, name: &RoleName) -> Result<Role, StorageError> {
        let row: Option<RoleRow> =
            sqlx::query_as("SELECT id, name, display_name, permissions FROM roles WHERE name = $1")
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
        // ON CONFLICT: idempotent — assigning a role twice is a no-op.
        sqlx::query(
            "INSERT INTO user_roles (user_id, role_id) VALUES ($1, $2)
             ON CONFLICT (user_id, role_id) DO NOTHING",
        )
        .bind(user_id.into_uuid())
        .bind(role_id.into_uuid())
        .execute(self.pool)
        .await?;
        Ok(())
    }

    async fn revoke_from_user(&self, user_id: UserId, role_id: RoleId) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM user_roles WHERE user_id = $1 AND role_id = $2")
            .bind(user_id.into_uuid())
            .bind(role_id.into_uuid())
            .execute(self.pool)
            .await?;
        Ok(())
    }

    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<Role>, StorageError> {
        let rows: Vec<RoleRow> = sqlx::query_as(
            "SELECT r.id, r.name, r.display_name, r.permissions
             FROM roles r
             JOIN user_roles ur ON ur.role_id = r.id
             WHERE ur.user_id = $1
             ORDER BY r.name ASC",
        )
        .bind(user_id.into_uuid())
        .fetch_all(self.pool)
        .await?;

        rows.into_iter().map(row_to_role).collect()
    }

    async fn list_roles_for_users(
        &self,
        user_ids: &[UserId],
    ) -> Result<HashMap<UserId, Vec<Role>>, StorageError> {
        let mut out: HashMap<UserId, Vec<Role>> = HashMap::with_capacity(user_ids.len());
        for uid in user_ids {
            out.entry(*uid).or_default();
        }
        if user_ids.is_empty() {
            return Ok(out);
        }
        // Postgres has native array binding, so the IN list is just one
        // parameter (no QueryBuilder dance like sqlite needs).
        let uuids: Vec<Uuid> = user_ids.iter().map(|u| u.into_uuid()).collect();
        let rows: Vec<(Uuid, Uuid, String, String, i64)> = sqlx::query_as(
            "SELECT ur.user_id, r.id, r.name, r.display_name, r.permissions
             FROM roles r
             JOIN user_roles ur ON ur.role_id = r.id
             WHERE ur.user_id = ANY($1)
             ORDER BY ur.user_id, r.name ASC",
        )
        .bind(&uuids)
        .fetch_all(self.pool)
        .await?;

        for (user_uuid, id, name, display_name, permissions) in rows {
            let user_id = UserId::from_uuid(user_uuid);
            let role = role_from_row(id, name, display_name, permissions)?;
            out.entry(user_id).or_default().push(role);
        }
        Ok(out)
    }

    async fn update(&self, role: &Role) -> Result<(), StorageError> {
        let permissions = permissions_to_i64(role.permissions);
        let out = sqlx::query(
            "UPDATE roles SET display_name = $1, permissions = $2 WHERE id = $3",
        )
        .bind(&role.display_name)
        .bind(permissions)
        .bind(role.id.into_uuid())
        .execute(self.pool)
        .await?;
        if out.rows_affected() == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn delete(&self, id: RoleId) -> Result<(), StorageError> {
        // user_roles.role_id is ON DELETE CASCADE; refuse here when the role
        // is still assigned to a user so the cascade doesn't silently
        // disconnect them. Folding the assignment check into the DELETE
        // statement keeps the precondition race-safe — a separate
        // `count_users` + `DELETE` would admit a concurrent assignment
        // between the two statements.
        let out = sqlx::query(
            "DELETE FROM roles r
             WHERE r.id = $1
               AND NOT EXISTS (SELECT 1 FROM user_roles ur WHERE ur.role_id = r.id)",
        )
        .bind(id.into_uuid())
        .execute(self.pool)
        .await?;
        if out.rows_affected() != 0 {
            return Ok(());
        }
        // Zero rows affected — disambiguate 404 vs 409.
        let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM roles WHERE id = $1")
            .bind(id.into_uuid())
            .fetch_optional(self.pool)
            .await?;
        if exists.is_some() {
            Err(StorageError::conflict(
                "role is still assigned to one or more users",
            ))
        } else {
            Err(StorageError::NotFound)
        }
    }

    async fn count_users(&self, id: RoleId) -> Result<u64, StorageError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM user_roles WHERE role_id = $1")
            .bind(id.into_uuid())
            .fetch_one(self.pool)
            .await?;
        #[allow(
            clippy::cast_sign_loss,
            reason = "COUNT(*) is non-negative"
        )]
        let count = if row.0 < 0 { 0 } else { row.0 as u64 };
        Ok(count)
    }
}
