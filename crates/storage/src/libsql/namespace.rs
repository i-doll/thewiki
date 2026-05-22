//! libsql [`NamespaceRepository`](crate::repo::NamespaceRepository) impl.

use libsql::{Connection, Value, params};
use thewiki_core::{Namespace, NamespaceId, NamespaceSlug};

use crate::error::StorageError;
use crate::libsql::codec::{
    format_ts, into_db, map_unique_violation, namespace_from_libsql_row, uuid_bytes,
};
use crate::repo::NamespaceRepository;

/// libsql-backed namespace repository.
pub struct LibsqlNamespaceRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LibsqlNamespaceRepository<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }
}

impl NamespaceRepository for LibsqlNamespaceRepository<'_> {
    async fn create(&self, namespace: &Namespace) -> Result<(), StorageError> {
        // Match the SQLite adapter: the schema demands a `created_at` but the
        // domain `Namespace` doesn't carry one yet, so stamp "now" at insert
        // time. Swap for the carried value once `Namespace` grows one.
        let now = format_ts(time::OffsetDateTime::now_utc())?;
        let id = uuid_bytes(namespace.id.into_uuid());

        let result = self
            .conn
            .execute(
                "INSERT INTO namespaces (id, slug, display_name, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![
                    Value::Blob(id.to_vec()),
                    namespace.slug.as_str().to_owned(),
                    namespace.display_name.clone(),
                    now,
                ],
            )
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(err, "namespace slug already in use")),
        }
    }

    async fn get_by_id(&self, id: NamespaceId) -> Result<Namespace, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, slug, display_name, created_at FROM namespaces WHERE id = ?1",
                    params![Value::Blob(id_bytes.to_vec())],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        match row {
            Some(row) => namespace_from_libsql_row(&row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn get_by_slug(&self, slug: &NamespaceSlug) -> Result<Namespace, StorageError> {
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, slug, display_name, created_at FROM namespaces WHERE slug = ?1",
                    params![slug.as_str().to_owned()],
                )
                .await,
        )?;
        let row = into_db(rows.next().await)?;
        match row {
            Some(row) => namespace_from_libsql_row(&row),
            None => Err(StorageError::NotFound),
        }
    }

    async fn list(&self) -> Result<Vec<Namespace>, StorageError> {
        let mut rows = into_db(
            self.conn
                .query(
                    "SELECT id, slug, display_name, created_at FROM namespaces ORDER BY created_at ASC, id ASC",
                    (),
                )
                .await,
        )?;
        let mut out = Vec::new();
        while let Some(row) = into_db(rows.next().await)? {
            out.push(namespace_from_libsql_row(&row)?);
        }
        Ok(out)
    }
}
