//! Integration coverage for the Postgres audit-log repository.

#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common_pg;

use common_pg::fresh_storage;
use serde_json::json;
use thewiki_core::{PageId, UserId};
use thewiki_storage::StorageError;
use thewiki_storage::repo::{AuditLogFilter, AuditLogRepository, Cursor, NewAuditLogEntry};
use time::{Duration, OffsetDateTime};

fn entry(actor_username: &str, action: &str, page: PageId) -> NewAuditLogEntry {
    NewAuditLogEntry {
        actor_id: UserId::new(),
        actor_username: actor_username.to_owned(),
        action: action.to_owned(),
        target_kind: "page".to_owned(),
        target_id: page.into_uuid(),
        target_label: Some(format!("Main/{action}")),
        metadata: json!({ "action": action }),
    }
}

#[tokio::test]
async fn create_then_list_round_trips_newest_first() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let page = PageId::new();
    let first = storage
        .audit_log()
        .create(entry("alice", "page.create", page))
        .await
        .expect("create first");
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let second = storage
        .audit_log()
        .create(entry("bob", "page.update", page))
        .await
        .expect("create second");

    let listed = storage
        .audit_log()
        .list(AuditLogFilter::default(), None, 10)
        .await
        .expect("list");

    assert_eq!(listed.items.len(), 2);
    assert_eq!(listed.items[0].id, second.id);
    assert_eq!(listed.items[1].id, first.id);
    assert!(listed.next.is_none());
    assert_eq!(listed.items[0].metadata, json!({ "action": "page.update" }));
}

#[tokio::test]
async fn list_filters_by_actor_action_and_time_range() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let page = PageId::new();
    storage
        .audit_log()
        .create(entry("alice", "page.create", page))
        .await
        .expect("alice create");
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let cutoff = OffsetDateTime::now_utc() - Duration::milliseconds(1);
    storage
        .audit_log()
        .create(entry("bob", "page.update", page))
        .await
        .expect("bob update");

    let listed = storage
        .audit_log()
        .list(
            AuditLogFilter {
                actor_username: Some("bob".to_string()),
                action: Some("page.update".to_string()),
                since: Some(cutoff),
                until: Some(OffsetDateTime::now_utc() + Duration::seconds(1)),
            },
            None,
            10,
        )
        .await
        .expect("filtered list");

    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].actor_username, "bob");
    assert_eq!(listed.items[0].action, "page.update");
}

#[tokio::test]
async fn list_paginates_with_cursor() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let page = PageId::new();
    for n in 0..3 {
        storage
            .audit_log()
            .create(entry("alice", &format!("page.{n}"), page))
            .await
            .expect("create");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }

    let first = storage
        .audit_log()
        .list(AuditLogFilter::default(), None, 2)
        .await
        .expect("first page");
    assert_eq!(first.items.len(), 2);
    let cursor = first.next.expect("next cursor");

    let second = storage
        .audit_log()
        .list(AuditLogFilter::default(), Some(cursor), 2)
        .await
        .expect("second page");
    assert_eq!(second.items.len(), 1);
    assert!(second.next.is_none());
}

#[tokio::test]
async fn malformed_cursor_is_invalid_input() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let err = storage
        .audit_log()
        .list(
            AuditLogFilter::default(),
            Some(Cursor("not-a-cursor".to_string())),
            10,
        )
        .await
        .expect_err("invalid cursor");
    assert!(matches!(err, StorageError::InvalidInput(_)), "got {err:?}");
}

#[tokio::test]
async fn prune_before_removes_old_rows() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let page = PageId::new();
    storage
        .audit_log()
        .create(entry("alice", "page.create", page))
        .await
        .expect("create old");
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    let cutoff = OffsetDateTime::now_utc();
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    storage
        .audit_log()
        .create(entry("alice", "page.update", page))
        .await
        .expect("create new");

    let pruned = storage
        .audit_log()
        .prune_before(cutoff)
        .await
        .expect("prune");
    assert_eq!(pruned, 1);

    let listed = storage
        .audit_log()
        .list(AuditLogFilter::default(), None, 10)
        .await
        .expect("list");
    assert_eq!(listed.items.len(), 1);
    assert_eq!(listed.items[0].action, "page.update");
}
