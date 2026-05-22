//! Transactional page mutations paired with audit-log writes.

use sqlx::sqlite::SqliteQueryResult;
use sqlx::{Sqlite, Transaction};
use thewiki_core::{AuditLogId, Page};
use time::OffsetDateTime;

use crate::error::StorageError;
use crate::repo::{NewAuditLogEntry, PageAuditMutation};
use crate::sqlite::audit_log::format_audit_ts;
use crate::sqlite::codec::{format_ts, map_unique_violation, uuid_bytes};

pub(super) async fn commit_page_audit(
    pool: &sqlx::SqlitePool,
    mutation: PageAuditMutation,
    audit: NewAuditLogEntry,
) -> Result<(), StorageError> {
    validate_mutation(&mutation)?;

    let mut tx = pool.begin().await?;
    match mutation {
        PageAuditMutation::CreatePage {
            mut page,
            live_revision,
        } => {
            let promoted_revision = live_revision.as_ref().map(|revision| revision.id);
            page.current_revision_id = None;
            insert_page(&mut tx, &page).await?;

            if let Some(revision) = live_revision {
                insert_revision(&mut tx, &revision).await?;
                page.current_revision_id = Some(revision.id);
                update_page(&mut tx, &page).await?;
            }
            debug_assert_eq!(page.current_revision_id, promoted_revision);
        }
        PageAuditMutation::CommitRevision { page, revision } => {
            insert_revision(&mut tx, &revision).await?;
            update_page(&mut tx, &page).await?;
        }
        PageAuditMutation::DeletePage { page_id } => {
            let result = delete_page(&mut tx, page_id).await?;
            if result.rows_affected() == 0 {
                return Err(StorageError::NotFound);
            }
        }
        PageAuditMutation::AuditOnly => {}
    }

    insert_audit(&mut tx, audit).await?;
    tx.commit().await?;
    Ok(())
}

fn validate_mutation(mutation: &PageAuditMutation) -> Result<(), StorageError> {
    match mutation {
        PageAuditMutation::CreatePage {
            page,
            live_revision: Some(revision),
        } => {
            if revision.page_id != page.id {
                return Err(StorageError::invalid_input(
                    "live page create revision must belong to the page",
                ));
            }
            if page.current_revision_id != Some(revision.id) {
                return Err(StorageError::invalid_input(
                    "live page create must promote the supplied revision",
                ));
            }
        }
        PageAuditMutation::CreatePage {
            page,
            live_revision: None,
        } => {
            if page.current_revision_id.is_some() {
                return Err(StorageError::invalid_input(
                    "queued page create must not have a current revision",
                ));
            }
        }
        PageAuditMutation::CommitRevision { page, revision } => {
            if revision.page_id != page.id {
                return Err(StorageError::invalid_input(
                    "page revision commit must use a revision for the page",
                ));
            }
            if page.current_revision_id != Some(revision.id) {
                return Err(StorageError::invalid_input(
                    "page revision commit must promote the supplied revision",
                ));
            }
        }
        PageAuditMutation::DeletePage { .. } | PageAuditMutation::AuditOnly => {}
    }
    Ok(())
}

async fn insert_page(tx: &mut Transaction<'_, Sqlite>, page: &Page) -> Result<(), StorageError> {
    let id = uuid_bytes(page.id.into_uuid());
    let namespace_id = uuid_bytes(page.namespace_id.into_uuid());
    let current_rev = page.current_revision_id.map(|r| uuid_bytes(r.into_uuid()));
    let created_at = format_ts(page.created_at)?;
    let updated_at = format_ts(page.updated_at)?;

    let result = sqlx::query(
        "INSERT INTO pages
            (id, namespace_id, slug, title, current_revision_id,
             content_format, protection_level, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )
    .bind(id.as_slice())
    .bind(namespace_id.as_slice())
    .bind(&page.slug)
    .bind(&page.title)
    .bind(current_rev.as_ref().map(|b| b.as_slice()))
    .bind(page.content_format.as_str())
    .bind(page.protection_level.as_str())
    .bind(&created_at)
    .bind(&updated_at)
    .execute(&mut **tx)
    .await;

    match result {
        Ok(_) => Ok(()),
        Err(err) => Err(map_unique_violation(
            err,
            "page slug already exists in namespace",
        )),
    }
}

async fn update_page(tx: &mut Transaction<'_, Sqlite>, page: &Page) -> Result<(), StorageError> {
    let id = uuid_bytes(page.id.into_uuid());
    let current_rev = page.current_revision_id.map(|r| uuid_bytes(r.into_uuid()));
    let updated_at = format_ts(page.updated_at)?;

    let result = sqlx::query(
        "UPDATE pages
         SET slug = ?1,
             title = ?2,
             current_revision_id = ?3,
             content_format = ?4,
             protection_level = ?5,
             updated_at = ?6
         WHERE id = ?7",
    )
    .bind(&page.slug)
    .bind(&page.title)
    .bind(current_rev.as_ref().map(|b| b.as_slice()))
    .bind(page.content_format.as_str())
    .bind(page.protection_level.as_str())
    .bind(&updated_at)
    .bind(id.as_slice())
    .execute(&mut **tx)
    .await;

    match result {
        Ok(out) => {
            if out.rows_affected() == 0 {
                Err(StorageError::NotFound)
            } else {
                Ok(())
            }
        }
        Err(err) => Err(map_unique_violation(
            err,
            "page slug already exists in namespace",
        )),
    }
}

async fn delete_page(
    tx: &mut Transaction<'_, Sqlite>,
    page_id: thewiki_core::PageId,
) -> Result<SqliteQueryResult, StorageError> {
    let id = uuid_bytes(page_id.into_uuid());
    Ok(sqlx::query("DELETE FROM pages WHERE id = ?1")
        .bind(id.as_slice())
        .execute(&mut **tx)
        .await?)
}

async fn insert_revision(
    tx: &mut Transaction<'_, Sqlite>,
    revision: &thewiki_core::Revision,
) -> Result<(), StorageError> {
    let id = uuid_bytes(revision.id.into_uuid());
    let page_id = uuid_bytes(revision.page_id.into_uuid());
    let parent_id = revision.parent_id.map(|r| uuid_bytes(r.into_uuid()));
    let author_id = uuid_bytes(revision.author_id.into_uuid());
    let created_at = format_ts(revision.created_at)?;

    sqlx::query(
        "INSERT INTO revisions
            (id, page_id, parent_id, author_id, body, edit_summary, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )
    .bind(id.as_slice())
    .bind(page_id.as_slice())
    .bind(parent_id.as_ref().map(|b| b.as_slice()))
    .bind(author_id.as_slice())
    .bind(&revision.body)
    .bind(revision.edit_summary.as_deref())
    .bind(&created_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_audit(
    tx: &mut Transaction<'_, Sqlite>,
    entry: NewAuditLogEntry,
) -> Result<(), StorageError> {
    let id = AuditLogId::new();
    let created_at = format_audit_ts(OffsetDateTime::now_utc())?;
    let id_bytes = uuid_bytes(id.into_uuid());
    let actor_bytes = uuid_bytes(entry.actor_id.into_uuid());
    let target_bytes = uuid_bytes(entry.target_id);
    let metadata = serde_json::to_string(&entry.metadata)
        .map_err(|err| StorageError::invalid_input(format!("audit metadata: {err}")))?;

    sqlx::query(
        "INSERT INTO audit_log
            (id, actor_id, actor_username, action, target_kind, target_id,
             target_label, metadata, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )
    .bind(id_bytes.as_slice())
    .bind(actor_bytes.as_slice())
    .bind(&entry.actor_username)
    .bind(&entry.action)
    .bind(&entry.target_kind)
    .bind(target_bytes.as_slice())
    .bind(&entry.target_label)
    .bind(&metadata)
    .bind(&created_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
