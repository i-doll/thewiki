//! Integration coverage for [`SqliteUserRepository`].

#![cfg(feature = "sqlite")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{fresh_storage, make_user};
use thewiki_core::{EmailAddress, Username};
use thewiki_storage::StorageError;
use thewiki_storage::repo::UserRepository;

#[tokio::test]
async fn create_then_get_round_trips() {
    let storage = fresh_storage().await;
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
    let storage = fresh_storage().await;
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
    let storage = fresh_storage().await;
    let a = make_user("alice");
    storage.users().create(&a, None).await.expect("first");

    // Different ID, same handle.
    let mut b = make_user("alice");
    b.id = thewiki_core::UserId::new();
    let err = storage.users().create(&b, None).await.expect_err("dup");
    assert!(matches!(err, StorageError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn update_persists_mutable_fields() {
    let storage = fresh_storage().await;
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
    let storage = fresh_storage().await;
    let user = make_user("alice");
    storage.users().create(&user, None).await.expect("create");

    storage.users().delete(user.id).await.expect("delete");
    let err = storage.users().get_by_id(user.id).await.expect_err("gone");
    assert!(matches!(err, StorageError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn delete_missing_user_is_not_found() {
    let storage = fresh_storage().await;
    let ghost = thewiki_core::UserId::new();
    let err = storage.users().delete(ghost).await.expect_err("not found");
    assert!(matches!(err, StorageError::NotFound), "got {err:?}");
}

#[tokio::test]
async fn password_hash_is_stored_opaque() {
    // Storage doesn't interpret the hash; we just check the column round-trips.
    let storage = fresh_storage().await;
    let user = make_user("alice");
    storage
        .users()
        .create(&user, Some("$argon2id$opaque-bytes"))
        .await
        .expect("create with hash");

    // Pull the column directly — there's no public getter and the repo trait
    // intentionally keeps password material out of the domain model.
    let pool = storage.pool();
    let id_bytes = *user.id.as_uuid().as_bytes();
    let (hash,): (Option<String>,) =
        sqlx::query_as("SELECT password_hash FROM users WHERE id = ?1")
            .bind(id_bytes.as_slice())
            .fetch_one(pool)
            .await
            .expect("fetch hash column");
    assert_eq!(hash.as_deref(), Some("$argon2id$opaque-bytes"));
}
