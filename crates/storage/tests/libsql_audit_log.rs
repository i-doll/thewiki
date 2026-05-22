//! Integration coverage for the libsql audit-log repository.

#![cfg(feature = "libsql")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common_libsql;

use common_libsql::{fresh_storage, make_namespace, make_page, make_user};
use serde_json::json;
use thewiki_core::{PageId, Revision, UserId};
use thewiki_storage::StorageError;
use thewiki_storage::repo::{
    AuditLogFilter, AuditLogRepository, Cursor, NamespaceRepository, NewAuditLogEntry,
    PageAuditMutation, PageRepository, RevisionRepository, UserRepository,
};
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
    let storage = fresh_storage().await;
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
    let storage = fresh_storage().await;
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
    let storage = fresh_storage().await;
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
    let storage = fresh_storage().await;
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
    let storage = fresh_storage().await;
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

#[tokio::test]
async fn commit_page_audit_creates_page_revision_and_audit_in_one_operation() {
    let storage = fresh_storage().await;
    let namespace = make_namespace("Main");
    storage
        .namespaces()
        .create(&namespace)
        .await
        .expect("namespace");
    let user = make_user("alice");
    storage.users().create(&user, None).await.expect("user");

    let mut page = make_page(namespace.id, "home");
    let revision = Revision::new(page.id, None, user.id, "body".to_string(), None);
    page.current_revision_id = Some(revision.id);
    page.updated_at = OffsetDateTime::now_utc();
    let audit = NewAuditLogEntry {
        actor_id: user.id,
        actor_username: user.username.as_str().to_owned(),
        action: "page.create".to_string(),
        target_kind: "page".to_string(),
        target_id: page.id.into_uuid(),
        target_label: Some("Main/home".to_string()),
        metadata: json!({ "revision_id": revision.id.into_uuid() }),
    };

    storage
        .commit_page_audit(
            PageAuditMutation::CreatePage {
                page: page.clone(),
                live_revision: Some(revision.clone()),
            },
            audit,
        )
        .await
        .expect("commit page audit");

    let stored_page = storage.pages().get_by_id(page.id).await.expect("page");
    let stored_revision = storage
        .revisions()
        .get_by_id(revision.id)
        .await
        .expect("revision");
    let audit_rows = storage
        .audit_log()
        .list(AuditLogFilter::default(), None, 10)
        .await
        .expect("audit rows");

    assert_eq!(stored_page.current_revision_id, Some(revision.id));
    assert_eq!(stored_revision.body, "body");
    assert_eq!(audit_rows.items.len(), 1);
    assert_eq!(audit_rows.items[0].action, "page.create");
}
