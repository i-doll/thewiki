//! SQLite [`RoleRepository`](crate::repo::RoleRepository) impl.

use std::collections::HashMap;

use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use thewiki_core::{Role, RoleId, RoleName, UserId};

use crate::error::StorageError;
use crate::repo::RoleRepository;
use crate::sqlite::codec::{
    decode_uuid, map_unique_violation, permissions_to_i64, role_from_row, uuid_bytes,
};

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

    async fn list_roles_for_users(
        &self,
        user_ids: &[UserId],
    ) -> Result<HashMap<UserId, Vec<Role>>, StorageError> {
        // Seed the result with every requested user mapped to an empty
        // Vec so callers can iterate the input list without a follow-up
        // existence check. Users with no roles never appear in the JOIN
        // below; this is the only place they're populated.
        let mut out: HashMap<UserId, Vec<Role>> = HashMap::with_capacity(user_ids.len());
        for uid in user_ids {
            out.entry(*uid).or_default();
        }
        if user_ids.is_empty() {
            return Ok(out);
        }

        // SQLite has no array type; emit a parameterised
        // `user_id IN (?, ?, ...)` list via `QueryBuilder::push_tuples`
        // so each id stays bound (no string interpolation).
        let mut builder: QueryBuilder<'_, Sqlite> = QueryBuilder::new(
            "SELECT ur.user_id, r.id, r.name, r.display_name, r.permissions
             FROM roles r
             JOIN user_roles ur ON ur.role_id = r.id
             WHERE ur.user_id IN (",
        );
        let mut separated = builder.separated(", ");
        for uid in user_ids {
            separated.push_bind(uuid_bytes(uid.into_uuid()).to_vec());
        }
        separated.push_unseparated(")");
        builder.push(" ORDER BY ur.user_id, r.name ASC");

        let rows = builder
            .build_query_as::<(
                Vec<u8>, // user_id
                Vec<u8>, // role.id
                String,  // role.name
                String,  // role.display_name
                i64,     // role.permissions
            )>()
            .fetch_all(self.pool)
            .await?;

        for (user_bytes, id, name, display_name, permissions) in rows {
            let user_id = UserId::from_uuid(decode_uuid(&user_bytes)?);
            let role = role_from_row(id, name, display_name, permissions)?;
            // The seed above guarantees the entry exists; using
            // `entry().or_default()` here would silently absorb a row
            // for an unknown id (which can't happen given the IN list
            // is exactly `user_ids`) but keeps the code defensively
            // correct.
            out.entry(user_id).or_default().push(role);
        }
        Ok(out)
    }

    async fn update(&self, role: &Role) -> Result<(), StorageError> {
        let id = uuid_bytes(role.id.into_uuid());
        let permissions = permissions_to_i64(role.permissions);
        let result = sqlx::query(
            "UPDATE roles SET display_name = ?1, permissions = ?2 WHERE id = ?3",
        )
        .bind(&role.display_name)
        .bind(permissions)
        .bind(id.as_slice())
        .execute(self.pool)
        .await?;
        if result.rows_affected() == 0 {
            Err(StorageError::NotFound)
        } else {
            Ok(())
        }
    }

    async fn delete(&self, id: RoleId) -> Result<(), StorageError> {
        // The `user_roles.role_id` FK is `ON DELETE CASCADE` in the
        // schema, so deleting a role with assignments would silently
        // detach all users from it. That's a footgun.
        //
        // Folding the assignment check into the DELETE itself makes the
        // operation race-safe — a check-then-act sequence
        // (`count_users` + `DELETE`) admits a concurrent assignment that
        // would land between the two statements and still cascade-detach
        // users when the DELETE fires. The `NOT EXISTS` predicate makes
        // the precondition part of the same statement.
        let id_bytes = uuid_bytes(id.into_uuid());
        let out = sqlx::query(
            "DELETE FROM roles
             WHERE id = ?1
               AND NOT EXISTS (SELECT 1 FROM user_roles WHERE role_id = ?1)",
        )
        .bind(id_bytes.as_slice())
        .execute(self.pool)
        .await?;
        if out.rows_affected() != 0 {
            return Ok(());
        }
        // Zero rows affected means either the role doesn't exist or it
        // still has assignments. Disambiguate so the API layer can pick
        // 404 vs 409.
        let exists: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM roles WHERE id = ?1")
            .bind(id_bytes.as_slice())
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
        let id_bytes = uuid_bytes(id.into_uuid());
        let row: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM user_roles WHERE role_id = ?1")
                .bind(id_bytes.as_slice())
                .fetch_one(self.pool)
                .await?;
        // Counts are non-negative by construction.
        #[allow(
            clippy::cast_sign_loss,
            reason = "COUNT(*) is non-negative; representation in sqlite is i64"
        )]
        let count = if row.0 < 0 { 0 } else { row.0 as u64 };
        Ok(count)
    }
}
