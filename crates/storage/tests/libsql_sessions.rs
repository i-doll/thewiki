//! Integration coverage for [`LibsqlSessionRepository`].

#![cfg(feature = "libsql")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common_libsql;

use std::time::Duration;

use common_libsql::{fresh_storage, make_user};
use thewiki_core::SessionId;
use thewiki_storage::StorageError;
use thewiki_storage::repo::{SessionRepository, UserRepository};

#[tokio::test]
async fn create_then_get_round_trips() {
    let storage = fresh_storage().await;
    let user = make_user("alice");
    storage.users().create(&user, None).await.expect("user");

    let session = storage
        .sessions()
        .create(
            user.id,
            Duration::from_secs(60),
            Some("curl"),
            Some("127.0.0.1"),
        )
        .await
        .expect("create session");
    assert_eq!(session.user_id, user.id);
    assert_eq!(session.user_agent.as_deref(), Some("curl"));
    assert_eq!(session.ip_address.as_deref(), Some("127.0.0.1"));

    let loaded = storage
        .sessions()
        .get_by_id(session.id)
        .await
        .expect("get session");
    assert_eq!(loaded.id, session.id);
    assert_eq!(loaded.user_id, user.id);
}

#[tokio::test]
async fn unknown_session_id_is_not_found() {
    let storage = fresh_storage().await;
    let err = storage
        .sessions()
        .get_by_id(SessionId::new())
        .await
        .expect_err("unknown");
    assert!(matches!(err, StorageError::NotFound));
}

#[tokio::test]
async fn expired_session_is_reported_as_not_found() {
    let storage = fresh_storage().await;
    let user = make_user("alice");
    storage.users().create(&user, None).await.expect("user");

    let session = storage
        .sessions()
        .create(user.id, Duration::from_millis(1), None, None)
        .await
        .expect("create");
    tokio::time::sleep(Duration::from_millis(20)).await;

    let err = storage
        .sessions()
        .get_by_id(session.id)
        .await
        .expect_err("expired");
    assert!(matches!(err, StorageError::NotFound));
}

#[tokio::test]
async fn touch_bumps_last_seen_at() {
    let storage = fresh_storage().await;
    let user = make_user("alice");
    storage.users().create(&user, None).await.expect("user");
    let session = storage
        .sessions()
        .create(user.id, Duration::from_secs(60), None, None)
        .await
        .expect("create");

    let before = session.last_seen_at;
    tokio::time::sleep(Duration::from_millis(20)).await;
    storage.sessions().touch(session.id).await.expect("touch");

    let loaded = storage.sessions().get_by_id(session.id).await.expect("get");
    assert!(
        loaded.last_seen_at > before,
        "expected {} > {before}",
        loaded.last_seen_at,
    );
}

#[tokio::test]
async fn delete_removes_row() {
    let storage = fresh_storage().await;
    let user = make_user("alice");
    storage.users().create(&user, None).await.expect("user");
    let session = storage
        .sessions()
        .create(user.id, Duration::from_secs(60), None, None)
        .await
        .expect("create");
    storage.sessions().delete(session.id).await.expect("delete");
    let err = storage
        .sessions()
        .get_by_id(session.id)
        .await
        .expect_err("gone");
    assert!(matches!(err, StorageError::NotFound));
}

#[tokio::test]
async fn delete_for_user_cascades_to_every_session() {
    let storage = fresh_storage().await;
    let user = make_user("alice");
    storage.users().create(&user, None).await.expect("user");
    for _ in 0..3 {
        storage
            .sessions()
            .create(user.id, Duration::from_secs(60), None, None)
            .await
            .expect("create");
    }

    let removed = storage
        .sessions()
        .delete_for_user(user.id)
        .await
        .expect("delete_for_user");
    assert_eq!(removed, 3);
}

#[tokio::test]
async fn prune_expired_only_removes_past_sessions() {
    let storage = fresh_storage().await;
    let user = make_user("alice");
    storage.users().create(&user, None).await.expect("user");

    storage
        .sessions()
        .create(user.id, Duration::from_millis(1), None, None)
        .await
        .expect("create expired");
    let kept = storage
        .sessions()
        .create(user.id, Duration::from_secs(60), None, None)
        .await
        .expect("create kept");
    tokio::time::sleep(Duration::from_millis(20)).await;

    let removed = storage.sessions().prune_expired().await.expect("prune");
    assert_eq!(removed, 1);
    assert!(storage.sessions().get_by_id(kept.id).await.is_ok());
}

#[tokio::test]
async fn session_cascades_when_user_is_deleted() {
    let storage = fresh_storage().await;
    let user = make_user("alice");
    storage.users().create(&user, None).await.expect("user");
    let session = storage
        .sessions()
        .create(user.id, Duration::from_secs(60), None, None)
        .await
        .expect("create");

    storage.users().delete(user.id).await.expect("delete user");
    let err = storage
        .sessions()
        .get_by_id(session.id)
        .await
        .expect_err("cascade should have removed the session");
    assert!(matches!(err, StorageError::NotFound));
}
