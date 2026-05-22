//! Small helpers for writing audit rows from page handlers.

use serde_json::Value;
use thewiki_core::{PageId, UserId};
use thewiki_storage::repo::{AuditLogRepository, NewAuditLogEntry};

use crate::error::ApiError;
use crate::state::{AppState, AppStorage};

/// Build one page-targeted audit event.
pub fn page_event(
    actor_id: UserId,
    actor_username: &str,
    action: &str,
    page_id: PageId,
    target_label: String,
    metadata: Value,
) -> NewAuditLogEntry {
    NewAuditLogEntry {
        actor_id,
        actor_username: actor_username.to_owned(),
        action: action.to_owned(),
        target_kind: "page".to_owned(),
        target_id: page_id.into_uuid(),
        target_label: Some(target_label),
        metadata,
    }
}

/// Persist one page-targeted audit event.
pub async fn record_page_event<S: AppStorage>(
    state: &AppState<S>,
    actor_id: UserId,
    actor_username: &str,
    action: &str,
    page_id: PageId,
    target_label: String,
    metadata: Value,
) -> Result<(), ApiError> {
    state
        .storage
        .audit_log()
        .create(page_event(
            actor_id,
            actor_username,
            action,
            page_id,
            target_label,
            metadata,
        ))
        .await?;
    Ok(())
}
