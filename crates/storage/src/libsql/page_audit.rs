//! Transactional page mutations paired with audit-log writes (libsql).

use libsql::{Connection, Transaction, Value, params};
use thewiki_core::{AuditLogId, Page};
use time::OffsetDateTime;

use crate::error::StorageError;
use crate::libsql::audit_log::format_audit_ts;
use crate::libsql::codec::{format_ts, into_db, map_unique_violation, opt_blob, uuid_bytes};
use crate::repo::{NewAuditLogEntry, PageAuditMutation};

pub(super) async fn commit_page_audit(
    conn: &Connection,
    mutation: PageAuditMutation,
    audit: NewAuditLogEntry,
) -> Result<(), StorageError> {
    validate_mutation(&mutation)?;

    let tx = into_db(conn.transaction().await)?;
    if let Err(err) = run_mutation(&tx, mutation).await {
        // Best-effort rollback; surface the original error either way.
        let _ = tx.rollback().await;
        return Err(err);
    }
    if let Err(err) = insert_audit(&tx, audit).await {
        let _ = tx.rollback().await;
        return Err(err);
    }
    into_db(tx.commit().await)?;
    Ok(())
}

async fn run_mutation(tx: &Transaction, mutation: PageAuditMutation) -> Result<(), StorageError> {
    match mutation {
        PageAuditMutation::CreatePage {
            mut page,
            live_revision,
        } => {
            let promoted_revision = live_revision.as_ref().map(|revision| revision.id);
            page.current_revision_id = None;
            insert_page(tx, &page).await?;

            if let Some(revision) = live_revision {
                insert_revision(tx, &revision).await?;
                page.current_revision_id = Some(revision.id);
                update_page(tx, &page).await?;
            }
            debug_assert_eq!(page.current_revision_id, promoted_revision);
        }
        PageAuditMutation::CommitRevision { page, revision } => {
            insert_revision(tx, &revision).await?;
            update_page(tx, &page).await?;
        }
        PageAuditMutation::UpdatePage { page } => {
            update_page(tx, &page).await?;
        }
        PageAuditMutation::DeletePage { page_id } => {
            let id = uuid_bytes(page_id.into_uuid());
            let rows_affected = into_db(
                tx.execute(
                    "DELETE FROM pages WHERE id = ?1",
                    params![Value::Blob(id.to_vec())],
                )
                .await,
            )?;
            if rows_affected == 0 {
                return Err(StorageError::NotFound);
            }
        }
        PageAuditMutation::AuditOnly => {}
    }
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
        PageAuditMutation::UpdatePage { .. }
        | PageAuditMutation::DeletePage { .. }
        | PageAuditMutation::AuditOnly => {}
    }
    Ok(())
}

async fn insert_page(tx: &Transaction, page: &Page) -> Result<(), StorageError> {
    let id = uuid_bytes(page.id.into_uuid());
    let namespace_id = uuid_bytes(page.namespace_id.into_uuid());
    let current_rev = page.current_revision_id.map(|r| uuid_bytes(r.into_uuid()));
    let created_at = format_ts(page.created_at)?;
    let updated_at = format_ts(page.updated_at)?;

    let result = tx
        .execute(
            "INSERT INTO pages
                (id, namespace_id, slug, title, current_revision_id,
                 content_format, protection_level, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                Value::Blob(id.to_vec()),
                Value::Blob(namespace_id.to_vec()),
                page.slug.clone(),
                page.title.clone(),
                opt_blob(current_rev.as_ref().map(|b| b.as_slice())),
                page.content_format.as_str().to_owned(),
                page.protection_level.as_str().to_owned(),
                created_at,
                updated_at,
            ],
        )
        .await;
    match result {
        Ok(_) => Ok(()),
        Err(err) => Err(map_unique_violation(
            err,
            "page slug already exists in namespace",
        )),
    }
}

async fn update_page(tx: &Transaction, page: &Page) -> Result<(), StorageError> {
    let id = uuid_bytes(page.id.into_uuid());
    let current_rev = page.current_revision_id.map(|r| uuid_bytes(r.into_uuid()));
    let updated_at = format_ts(page.updated_at)?;

    let result = tx
        .execute(
            "UPDATE pages
             SET slug = ?1,
                 title = ?2,
                 current_revision_id = ?3,
                 content_format = ?4,
                 protection_level = ?5,
                 updated_at = ?6
             WHERE id = ?7",
            params![
                page.slug.clone(),
                page.title.clone(),
                opt_blob(current_rev.as_ref().map(|b| b.as_slice())),
                page.content_format.as_str().to_owned(),
                page.protection_level.as_str().to_owned(),
                updated_at,
                Value::Blob(id.to_vec()),
            ],
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
        Err(err) => Err(map_unique_violation(
            err,
            "page slug already exists in namespace",
        )),
    }
}

async fn insert_revision(
    tx: &Transaction,
    revision: &thewiki_core::Revision,
) -> Result<(), StorageError> {
    let id = uuid_bytes(revision.id.into_uuid());
    let page_id = uuid_bytes(revision.page_id.into_uuid());
    let parent_id = revision.parent_id.map(|r| uuid_bytes(r.into_uuid()));
    let author_id = uuid_bytes(revision.author_id.into_uuid());
    let created_at = format_ts(revision.created_at)?;

    into_db(
        tx.execute(
            "INSERT INTO revisions
                (id, page_id, parent_id, author_id, body, edit_summary, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                Value::Blob(id.to_vec()),
                Value::Blob(page_id.to_vec()),
                opt_blob(parent_id.as_ref().map(|b| b.as_slice())),
                Value::Blob(author_id.to_vec()),
                revision.body.clone(),
                match revision.edit_summary.as_deref() {
                    Some(s) => Value::Text(s.to_owned()),
                    None => Value::Null,
                },
                created_at,
            ],
        )
        .await,
    )?;
    Ok(())
}

async fn insert_audit(tx: &Transaction, entry: NewAuditLogEntry) -> Result<(), StorageError> {
    let id = AuditLogId::new();
    let created_at = format_audit_ts(OffsetDateTime::now_utc())?;
    let id_bytes = uuid_bytes(id.into_uuid());
    let actor_bytes = uuid_bytes(entry.actor_id.into_uuid());
    let target_bytes = uuid_bytes(entry.target_id);
    let metadata = serde_json::to_string(&entry.metadata)
        .map_err(|err| StorageError::invalid_input(format!("audit metadata: {err}")))?;

    let binds: Vec<Value> = vec![
        Value::Blob(id_bytes.to_vec()),
        Value::Blob(actor_bytes.to_vec()),
        Value::Text(entry.actor_username.clone()),
        Value::Text(entry.action.clone()),
        Value::Text(entry.target_kind.clone()),
        Value::Blob(target_bytes.to_vec()),
        match entry.target_label.as_deref() {
            Some(s) => Value::Text(s.to_owned()),
            None => Value::Null,
        },
        Value::Text(metadata),
        Value::Text(created_at),
    ];

    into_db(
        tx.execute(
            "INSERT INTO audit_log
                (id, actor_id, actor_username, action, target_kind, target_id,
                 target_label, metadata, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            binds,
        )
        .await,
    )?;
    Ok(())
}
