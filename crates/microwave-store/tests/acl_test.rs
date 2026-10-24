use microwave_store::{Permission, SqliteStore};

fn user(store: &SqliteStore, name: &str) -> String {
    store
        .create_user(
            name,
            &format!("{name}@example.com"),
            "bcrypt",
            "h",
            "s",
            false,
        )
        .unwrap()
        .authz_id
}

#[test]
fn get_acl_lists_names_for_every_permission() {
    let store = SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("acme", "Acme Corp").unwrap();
    let target = store
        .create_container_for_org(&org.id, "nodes")
        .unwrap()
        .authz_id;
    let alice = user(&store, "alice");
    let bob = user(&store, "bob");
    let admins = store
        .create_group_for_org(&org.id, "admins")
        .unwrap()
        .authz_id;

    store
        .set_acl(&target, Permission::Read, Some(&alice), Some(&admins))
        .unwrap();
    store
        .set_acl(&target, Permission::Create, Some(&bob), None)
        .unwrap();

    let acl = store.get_acl(&target).unwrap();
    assert_eq!(acl.len(), 5);

    let read = acl
        .iter()
        .find(|(permission, _, _)| *permission == Permission::Read)
        .unwrap();
    assert_eq!(read.1, vec!["alice".to_string()]);
    assert_eq!(read.2, vec!["admins".to_string()]);

    let create = acl
        .iter()
        .find(|(permission, _, _)| *permission == Permission::Create)
        .unwrap();
    assert_eq!(create.1, vec!["bob".to_string()]);
    assert!(create.2.is_empty());

    let update = acl
        .iter()
        .find(|(permission, _, _)| *permission == Permission::Update)
        .unwrap();
    assert!(update.1.is_empty());
    assert!(update.2.is_empty());
}

#[test]
fn granular_acl_separates_users_and_clients() {
    let store = SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("acme", "Acme Corp").unwrap();
    let target = store
        .create_container_for_org(&org.id, "nodes")
        .unwrap()
        .authz_id;
    let alice = user(&store, "alice");
    let robot = store.create_client(&org.id, "robot", false, false).unwrap();
    let admins = store
        .create_group_for_org(&org.id, "admins")
        .unwrap()
        .authz_id;

    store
        .set_acl(&target, Permission::Read, Some(&alice), None)
        .unwrap();
    store
        .set_acl(
            &target,
            Permission::Update,
            Some(&robot.authz_id),
            Some(&admins),
        )
        .unwrap();

    let acl = store.get_acl_entries(&target).unwrap();
    let read = acl
        .iter()
        .find(|entry| entry.permission == Permission::Read)
        .unwrap();
    assert_eq!(read.users, vec!["alice".to_string()]);
    assert!(read.clients.is_empty());

    let update = acl
        .iter()
        .find(|entry| entry.permission == Permission::Update)
        .unwrap();
    assert!(update.users.is_empty());
    assert_eq!(update.clients, vec!["robot".to_string()]);
    assert_eq!(update.groups, vec!["admins".to_string()]);
}

#[test]
fn set_acl_for_permission_replaces_only_that_permission() {
    let store = SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("acme", "Acme Corp").unwrap();
    let target = store
        .create_container_for_org(&org.id, "nodes")
        .unwrap()
        .authz_id;
    let alice = user(&store, "alice");
    let _bob = user(&store, "bob");
    store.create_group_for_org(&org.id, "admins").unwrap();

    store
        .set_acl_for_permission(
            &target,
            Permission::Read,
            &["alice".to_string()],
            &["admins".to_string()],
        )
        .unwrap();
    store
        .set_acl(&target, Permission::Update, Some(&alice), None)
        .unwrap();

    store
        .set_acl_for_permission(&target, Permission::Read, &["bob".to_string()], &[])
        .unwrap();

    let acl = store.get_acl(&target).unwrap();
    let read = acl
        .iter()
        .find(|(permission, _, _)| *permission == Permission::Read)
        .unwrap();
    assert_eq!(read.1, vec!["bob".to_string()]);
    assert!(read.2.is_empty());

    let update = acl
        .iter()
        .find(|(permission, _, _)| *permission == Permission::Update)
        .unwrap();
    assert_eq!(update.1, vec!["alice".to_string()]);

    // The resolved ids remain usable for authorization checks.
    let bob_user = store.find_user("bob").unwrap().unwrap();
    let admins_group = store.find_group(&org.id, "admins").unwrap().unwrap();
    assert!(store
        .check_permission(&bob_user.authz_id, &target, Permission::Read)
        .unwrap());
    assert!(!store
        .check_permission(&bob_user.authz_id, &target, Permission::Update)
        .unwrap());

    store
        .set_acl_for_permission(&target, Permission::Read, &[], &["admins".to_string()])
        .unwrap();
    let alice_user = store.find_user("alice").unwrap().unwrap();
    store
        .add_group_member(&admins_group.authz_id, &alice_user.authz_id)
        .unwrap();
    assert!(store
        .check_permission(&alice_user.authz_id, &target, Permission::Read)
        .unwrap());
    assert!(!store
        .check_permission(&bob_user.authz_id, &target, Permission::Read)
        .unwrap());

    let _ = alice;
}

#[test]
fn set_acl_rejects_unknown_actor_and_group_names_atomically() {
    let store = SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("acme", "Acme Corp").unwrap();
    let target = store
        .create_container_for_org(&org.id, "nodes")
        .unwrap()
        .authz_id;
    let _alice = user(&store, "alice");

    store
        .set_acl_for_permission(&target, Permission::Read, &["alice".to_string()], &[])
        .unwrap();

    let error = store
        .set_acl_for_permission(&target, Permission::Read, &["nobody".to_string()], &[])
        .unwrap_err();
    assert!(matches!(error, microwave_store::StoreError::Invalid(_)));

    let error = store
        .set_acl_for_permission(
            &target,
            Permission::Read,
            &[],
            &["missing-group".to_string()],
        )
        .unwrap_err();
    assert!(matches!(error, microwave_store::StoreError::Invalid(_)));

    let acl = store.get_acl(&target).unwrap();
    let read = acl
        .iter()
        .find(|(permission, _, _)| *permission == Permission::Read)
        .unwrap();
    assert_eq!(read.1, vec!["alice".to_string()]);
}

#[test]
fn acl_target_must_exist() {
    let store = SqliteStore::open_in_memory().unwrap();

    let error = store.get_acl("does-not-exist").unwrap_err();
    assert!(matches!(error, microwave_store::StoreError::NotFound(_)));

    let error = store
        .set_acl_for_permission("does-not-exist", Permission::Read, &[], &[])
        .unwrap_err();
    assert!(matches!(error, microwave_store::StoreError::NotFound(_)));
}

#[test]
fn global_user_acl_only_accepts_users() {
    let store = SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("acme", "Acme Corp").unwrap();
    let alice = store
        .create_user("alice", "alice@example.com", "bcrypt", "h", "s", false)
        .unwrap();
    store
        .create_user("bob", "bob@example.com", "bcrypt", "h", "s", false)
        .unwrap();
    store.create_client(&org.id, "robot", false, false).unwrap();

    store
        .set_acl_for_permission(&alice.authz_id, Permission::Read, &["bob".to_string()], &[])
        .unwrap();

    let acl = store.get_acl(&alice.authz_id).unwrap();
    let read = acl
        .iter()
        .find(|(permission, _, _)| *permission == Permission::Read)
        .unwrap();
    assert_eq!(read.1, vec!["bob".to_string()]);

    let error = store
        .set_acl_for_permission(
            &alice.authz_id,
            Permission::Read,
            &["robot".to_string()],
            &[],
        )
        .unwrap_err();
    assert!(matches!(error, microwave_store::StoreError::Invalid(_)));
}
