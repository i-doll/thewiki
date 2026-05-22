//! Integration coverage for [`PostgresUserRepository`].
//!
//! Skipped when no Postgres URL is configured.

#![cfg(feature = "postgres")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common_pg;

use common_pg::{fresh_storage, make_user};
use thewiki_core::{EmailAddress, Username};
use thewiki_storage::StorageError;
use thewiki_storage::repo::UserRepository;

#[tokio::test]
async fn create_then_get_round_trips() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let user = make_user("alice");
    storage.users().create(&user, None).await.expect("create");

    let loaded = storage.users().get_by_id(user.id).await.expect("get");
    assert_eq!(loaded.id, user.id);
    assert_eq!(loaded.username, user.username);
    assert_eq!(loaded.email, user.email);
    assert_eq!(loaded.display_name, user.display_name);
}

#[tokio::test]
async fn get_by_username_resolves() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let user = make_user("bob");
    storage.users().create(&user, None).await.expect("create");

    let username = Username::new("bob").expect("valid");
    let loaded = storage
        .users()
        .get_by_username(&username)
        .await
        .expect("by username");
    assert_eq!(loaded.id, user.id);
}

#[tokio::test]
async fn duplicate_username_conflicts() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let a = make_user("alice");
    storage.users().create(&a, None).await.expect("first");

    let mut b = make_user("alice");
    b.id = thewiki_core::UserId::new();
    let err = storage.users().create(&b, None).await.expect_err("dup");
    assert!(matches!(err, StorageError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn update_persists_mutable_fields() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let mut user = make_user("alice");
    storage.users().create(&user, None).await.expect("create");

    user.display_name = Some("Alice Lovelace".into());
    user.email = Some(EmailAddress::new("alice.new@example.com").expect("email"));
    user.last_login_at = Some(time::OffsetDateTime::now_utc());
    storage.users().update(&user).await.expect("update");

    let loaded = storage.users().get_by_id(user.id).await.expect("get");
    assert_eq!(loaded.display_name.as_deref(), Some("Alice Lovelace"));
    assert_eq!(
        loaded.email.as_ref().map(|e| e.as_str()),
        Some("alice.new@example.com"),
    );
    assert!(loaded.last_login_at.is_some());
}

#[tokio::test]
async fn delete_removes_row() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let user = make_user("alice");
    storage.users().create(&user, None).await.expect("create");

    storage.users().delete(user.id).await.expect("delete");
    let err = storage.users().get_by_id(user.id).await.expect_err("gone");
    assert!(matches!(err, StorageError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn delete_missing_user_is_not_found() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let ghost = thewiki_core::UserId::new();
    let err = storage.users().delete(ghost).await.expect_err("not found");
    assert!(matches!(err, StorageError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn password_hash_is_stored_opaque() {
    let Some((storage, _keep)) = fresh_storage().await else {
        return;
    };
    let user = make_user("alice");
    storage
        .users()
        .create(&user, Some("$argon2id$opaque-bytes"))
        .await
        .expect("create with hash");

    let pool = storage.pool();
    let (hash,): (Option<String>,) =
        sqlx::query_as("SELECT password_hash FROM users WHERE id = $1")
            .bind(user.id.into_uuid())
            .fetch_one(pool)
            .await
            .expect("fetch hash column");
    assert_eq!(hash.as_deref(), Some("$argon2id$opaque-bytes"));
}
