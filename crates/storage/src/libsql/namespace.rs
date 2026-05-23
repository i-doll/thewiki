//! libsql [`NamespaceRepository`](crate::repo::NamespaceRepository) impl.

use libsql::{Connection, Transaction, Value, params};
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

/// Prefix prepended to a subject namespace slug to produce its discussion
/// counterpart (#43).
const TALK_SLUG_PREFIX: &str = "Talk_";

/// Prefix prepended to a subject namespace's display name to produce the
/// talk-side label.
const TALK_DISPLAY_PREFIX: &str = "Talk: ";

/// SELECT-list used by every read query. Keeps the column order in lockstep
/// with [`namespace_from_libsql_row`].
const NAMESPACE_COLUMNS: &str = "id, slug, display_name, created_at, is_talk, paired_namespace_id";

/// libsql-backed namespace repository.
pub struct LibsqlNamespaceRepository<'a> {
    conn: &'a Connection,
}

impl<'a> LibsqlNamespaceRepository<'a> {
    pub(super) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    fn build_talk_pair(subject: &Namespace) -> Result<Namespace, StorageError> {
        let talk_slug = format!("{TALK_SLUG_PREFIX}{}", subject.slug.as_str());
        let slug = NamespaceSlug::new(&talk_slug).map_err(|err| {
            StorageError::invalid_input(format!(
                "could not derive talk slug from {:?}: {err}",
                subject.slug.as_str()
            ))
        })?;
        Ok(Namespace {
            id: NamespaceId::new(),
            slug,
            display_name: format!("{TALK_DISPLAY_PREFIX}{}", subject.display_name),
            is_talk: true,
            paired_namespace_id: Some(subject.id),
        })
    }

    async fn insert_row_tx(
        tx: &Transaction,
        namespace: &Namespace,
    ) -> Result<(), StorageError> {
        let now = format_ts(time::OffsetDateTime::now_utc())?;
        let id = uuid_bytes(namespace.id.into_uuid());
        let paired_value: Value = match namespace.paired_namespace_id {
            Some(p) => Value::Blob(uuid_bytes(p.into_uuid()).to_vec()),
            None => Value::Null,
        };

        let result = tx
            .execute(
                "INSERT INTO namespaces (id, slug, display_name, created_at, is_talk, paired_namespace_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    Value::Blob(id.to_vec()),
                    namespace.slug.as_str().to_owned(),
                    namespace.display_name.clone(),
                    now,
                    i64::from(namespace.is_talk),
                    paired_value,
                ],
            )
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => Err(map_unique_violation(err, "namespace slug already in use")),
        }
    }

    async fn set_pair_tx(
        tx: &Transaction,
        id: NamespaceId,
        paired_id: NamespaceId,
    ) -> Result<(), StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let paired_bytes = uuid_bytes(paired_id.into_uuid());
        tx.execute(
            "UPDATE namespaces SET paired_namespace_id = ?1 WHERE id = ?2",
            params![
                Value::Blob(paired_bytes.to_vec()),
                Value::Blob(id_bytes.to_vec()),
            ],
        )
        .await
        .map_err(db_error)?;
        Ok(())
    }

    /// Lookup a namespace by slug inside the current transaction. Used by
    /// `create()` so the "Talk_<slug> already exists" branch reads the
    /// same view of the data the surrounding writes do.
    async fn get_by_slug_tx(
        tx: &Transaction,
        slug: &NamespaceSlug,
    ) -> Result<Namespace, StorageError> {
        let mut rows = into_db(
            tx.query(
                &format!("SELECT {NAMESPACE_COLUMNS} FROM namespaces WHERE slug = ?1"),
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
}

impl NamespaceRepository for LibsqlNamespaceRepository<'_> {
    async fn create(&self, namespace: &Namespace) -> Result<(), StorageError> {
        // The subject insert, the paired talk insert, and the bidirectional
        // `paired_namespace_id` updates all run inside a single transaction
        // so a failure on any later step rolls back the whole pairing
        // graph (#43, coderabbit). libsql's `Transaction` derefs to
        // `Connection`, so the inner methods share the same query API.
        let tx = into_db(self.conn.transaction().await)?;

        if let Err(err) = Self::insert_row_tx(&tx, namespace).await {
            let _ = tx.rollback().await;
            return Err(err);
        }

        if !namespace.is_talk && namespace.paired_namespace_id.is_none() {
            let talk = match Self::build_talk_pair(namespace) {
                Ok(talk) => talk,
                Err(err) => {
                    let _ = tx.rollback().await;
                    return Err(err);
                }
            };
            match Self::insert_row_tx(&tx, &talk).await {
                Ok(()) => {
                    if let Err(err) = Self::set_pair_tx(&tx, namespace.id, talk.id).await {
                        let _ = tx.rollback().await;
                        return Err(err);
                    }
                }
                Err(StorageError::Conflict(_)) => {
                    let existing = match Self::get_by_slug_tx(&tx, &talk.slug).await {
                        Ok(ns) => ns,
                        Err(err) => {
                            let _ = tx.rollback().await;
                            return Err(err);
                        }
                    };
                    if existing.paired_namespace_id.is_none()
                        && let Err(err) = Self::set_pair_tx(&tx, existing.id, namespace.id).await
                    {
                        let _ = tx.rollback().await;
                        return Err(err);
                    }
                    if let Err(err) = Self::set_pair_tx(&tx, namespace.id, existing.id).await {
                        let _ = tx.rollback().await;
                        return Err(err);
                    }
                }
                Err(err) => {
                    let _ = tx.rollback().await;
                    return Err(err);
                }
            }
        }
        into_db(tx.commit().await)?;
        Ok(())
    }

    async fn get_by_id(&self, id: NamespaceId) -> Result<Namespace, StorageError> {
        let id_bytes = uuid_bytes(id.into_uuid());
        let mut rows = into_db(
            self.conn
                .query(
                    &format!("SELECT {NAMESPACE_COLUMNS} FROM namespaces WHERE id = ?1"),
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
                    &format!("SELECT {NAMESPACE_COLUMNS} FROM namespaces WHERE slug = ?1"),
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
                    &format!(
                        "SELECT {NAMESPACE_COLUMNS} FROM namespaces ORDER BY created_at ASC, id ASC"
                    ),
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
                    is_talk: false,
                    paired_namespace_id: None,
                };
                match self.create(&ns).await {
                    Ok(()) => self.get_by_id(ns.id).await,
                    Err(StorageError::Conflict(_)) => self.get_by_slug(&ns.slug).await,
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }
}
