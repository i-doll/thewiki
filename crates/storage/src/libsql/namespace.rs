//! libsql [`NamespaceRepository`](crate::repo::NamespaceRepository) impl.

use libsql::{Connection, Value, params};
use thewiki_core::{Namespace, NamespaceId, NamespaceSlug};

use crate::error::StorageError;
use crate::libsql::codec::{
    db_error, format_ts, into_db, map_fk_restrict_violation, map_unique_violation,
    namespace_from_libsql_row, uuid_bytes,
};
use crate::repo::NamespaceRepository;

/// Slug used for the implicit default namespace seeded at boot (#28).
const DEFAULT_NAMESPACE_SLUG: &str = "Main";

/// Slug used for the implicit template namespace seeded at boot (#45).
const TEMPLATE_NAMESPACE_SLUG: &str = "Template";

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

    async fn update_display_name(
        &self,
        id: NamespaceId,
        display_name: &str,
    ) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let affected = self
            .conn
            .execute(
                "UPDATE namespaces SET display_name = ?1 WHERE id = ?2",
                params![display_name.to_owned(), Value::Blob(id_bytes.to_vec())],
            )
            .await
            .map_err(db_error)?;
        if affected == 0 {
            return Err(StorageError::NotFound);
        }
        Ok(())
    }

    async fn delete(&self, id: NamespaceId) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let result = self
            .conn
            .execute(
                "DELETE FROM namespaces WHERE id = ?1",
                params![Value::Blob(id_bytes.to_vec())],
            )
            .await;
        match result {
            Ok(0) => Err(StorageError::NotFound),
            Ok(_) => Ok(()),
            Err(err) => Err(map_fk_restrict_violation(
                err,
                "namespace still contains pages",
            )),
        }
    }

    async fn get_or_create_default(&self) -> Result<Namespace, StorageError> {
        self.get_or_create_by_slug(DEFAULT_NAMESPACE_SLUG).await
    }

    async fn get_or_create_template_namespace(&self) -> Result<Namespace, StorageError> {
        self.get_or_create_by_slug(TEMPLATE_NAMESPACE_SLUG).await
    }
}

impl LibsqlNamespaceRepository<'_> {
    /// Shared implementation of the idempotent "seed by slug" path used by
    /// [`get_or_create_default`](Self::get_or_create_default) and
    /// [`get_or_create_template_namespace`](Self::get_or_create_template_namespace).
    async fn get_or_create_by_slug(&self, slug_str: &str) -> Result<Namespace, StorageError> {
        let slug = NamespaceSlug::new(slug_str).map_err(|e| {
            StorageError::InvalidInput(format!("namespace slug {slug_str:?} is invalid: {e}"))
        })?;
        match self.get_by_slug(&slug).await {
            Ok(ns) => Ok(ns),
            Err(StorageError::NotFound) => {
                let ns = Namespace {
                    id: NamespaceId::new(),
                    slug,
                    display_name: slug_str.to_owned(),
                };
                match self.create(&ns).await {
                    Ok(()) => Ok(ns),
                    Err(StorageError::Conflict(_)) => self.get_by_slug(&ns.slug).await,
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }
}
