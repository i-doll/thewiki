//! Integration coverage for [`SqliteRoleRepository`].

#![cfg(feature = "sqlite")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{fresh_storage, make_role, make_user};
use thewiki_core::{Permissions, RoleName};
use thewiki_storage::StorageError;
use thewiki_storage::repo::{RoleRepository, UserRepository};

#[tokio::test]
async fn create_then_get_round_trips() {
    let storage = fresh_storage().await;
    let role = make_role("editor", Permissions::READ | Permissions::EDIT);
    storage.roles().create(&role).await.expect("create");

    let loaded = storage.roles().get_by_id(role.id).await.expect("by id");
    assert_eq!(loaded.id, role.id);
    assert_eq!(loaded.name, role.name);
    assert_eq!(loaded.permissions, role.permissions);

    let by_name_handle = RoleName::new("editor").expect("name");
    let by_name = storage
        .roles()
        .get_by_name(&by_name_handle)
        .await
        .expect("by name");
    assert_eq!(by_name.id, role.id);
}

#[tokio::test]
async fn duplicate_role_name_conflicts() {
    let storage = fresh_storage().await;
    let r1 = make_role("editor", Permissions::READ);
    storage.roles().create(&r1).await.expect("first");
    let r2 = make_role("editor", Permissions::EDIT);
    let err = storage.roles().create(&r2).await.expect_err("dup");
    assert!(matches!(err, StorageError::Conflict(_)), "got {err:?}");
}

#[tokio::test]
async fn list_returns_all_roles_sorted_by_name() {
    let storage = fresh_storage().await;
    storage
        .roles()
        .create(&make_role("user", Permissions::READ))
        .await
        .expect("user role");
    storage
        .roles()
        .create(&make_role("admin", Permissions::all()))
        .await
        .expect("admin role");
    storage
        .roles()
        .create(&make_role("editor", Permissions::READ | Permissions::EDIT))
        .await
        .expect("editor role");

    let all = storage.roles().list().await.expect("list");
    let names: Vec<_> = all.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["admin", "editor", "user"]);
}

#[tokio::test]
async fn assign_and_revoke_round_trips() {
    let storage = fresh_storage().await;
    let user = make_user("alice");
    let role = make_role("editor", Permissions::READ | Permissions::EDIT);
    storage.users().create(&user, None).await.expect("user");
    storage.roles().create(&role).await.expect("role");

    storage
        .roles()
        .assign_to_user(user.id, role.id)
        .await
        .expect("assign");

    let held = storage.roles().list_for_user(user.id).await.expect("list");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].id, role.id);

    // Idempotency: assigning a second time is fine.
    storage
        .roles()
        .assign_to_user(user.id, role.id)
        .await
        .expect("assign again");
    let still = storage.roles().list_for_user(user.id).await.expect("still");
    assert_eq!(still.len(), 1, "no duplicates");

    storage
        .roles()
        .revoke_from_user(user.id, role.id)
        .await
        .expect("revoke");
    let none = storage
        .roles()
        .list_for_user(user.id)
        .await
        .expect("post-revoke");
    assert!(none.is_empty());

    // Idempotency: revoking again is a no-op.
    storage
        .roles()
        .revoke_from_user(user.id, role.id)
        .await
        .expect("revoke again");
}

#[tokio::test]
async fn list_for_user_returns_only_their_roles() {
    let storage = fresh_storage().await;

    let alice = make_user("alice");
    let bob = make_user("bob");
    storage.users().create(&alice, None).await.expect("alice");
    storage.users().create(&bob, None).await.expect("bob");

    let editor = make_role("editor", Permissions::EDIT);
    let admin = make_role("admin", Permissions::all());
    storage.roles().create(&editor).await.expect("editor");
    storage.roles().create(&admin).await.expect("admin");

    storage
        .roles()
        .assign_to_user(alice.id, editor.id)
        .await
        .expect("alice/editor");
    storage
        .roles()
        .assign_to_user(bob.id, admin.id)
        .await
        .expect("bob/admin");

    let alices = storage
        .roles()
        .list_for_user(alice.id)
        .await
        .expect("alice's roles");
    assert_eq!(alices.len(), 1);
    assert_eq!(alices[0].name.as_str(), "editor");

    let bobs = storage
        .roles()
        .list_for_user(bob.id)
        .await
        .expect("bob's roles");
    assert_eq!(bobs.len(), 1);
    assert_eq!(bobs[0].name.as_str(), "admin");
}

#[tokio::test]
async fn permissions_round_trip_through_storage() {
    let storage = fresh_storage().await;
    let perms = Permissions::READ
        | Permissions::EDIT
        | Permissions::CREATE
        | Permissions::PROTECT
        | Permissions::MANAGE_USERS;
    let role = make_role("moderator", perms);
    storage.roles().create(&role).await.expect("create");

    let loaded = storage.roles().get_by_id(role.id).await.expect("get");
    assert_eq!(loaded.permissions, perms);
}
