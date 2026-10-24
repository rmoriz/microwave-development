use microwave_authz::{create_organization, Permission};
use microwave_store::SqliteStore;

#[test]
fn creates_default_containers_groups_and_acls() {
    let store = SqliteStore::open_in_memory().unwrap();
    let creator = store
        .create_user("root", "root@example.com", "bcrypt", "hash", "salt", true)
        .unwrap();
    let org =
        create_organization(&store, "acme", "Acme Corp", &creator.id, &creator.authz_id).unwrap();

    for name in microwave_authz::DEFAULT_CONTAINERS {
        assert!(
            store.find_container(&org.id, name).unwrap().is_some(),
            "missing container {name}"
        );
    }
    for name in microwave_authz::DEFAULT_GROUPS {
        assert!(
            store.find_group(&org.id, name).unwrap().is_some(),
            "missing group {name}"
        );
    }

    let nodes = store.find_container(&org.id, "nodes").unwrap().unwrap();
    assert!(store
        .check_permission(&creator.authz_id, &nodes.authz_id, Permission::Create)
        .unwrap());
    assert!(store
        .check_permission(&creator.authz_id, &nodes.authz_id, Permission::Read)
        .unwrap());
}

#[test]
fn default_environment_inherits_the_environments_container_acl() {
    let store = SqliteStore::open_in_memory().unwrap();
    let creator = store
        .create_user("root", "root@example.com", "bcrypt", "hash", "salt", true)
        .unwrap();
    let org =
        create_organization(&store, "acme", "Acme Corp", &creator.id, &creator.authz_id).unwrap();

    let environment = store
        .find_environment(&org.id, "_default")
        .unwrap()
        .expect("default environment must exist");
    let clients = store.find_group(&org.id, "clients").unwrap().unwrap();
    let client = store.create_client(&org.id, "node1", false, false).unwrap();
    store
        .add_group_member(&clients.authz_id, &client.authz_id)
        .unwrap();

    // A client is a member of the clients group, which has read on the
    // environments container. The default environment must inherit that.
    assert!(store
        .check_permission(&client.authz_id, &environment.authz_id, Permission::Read)
        .unwrap());
    assert!(!store
        .check_permission(&client.authz_id, &environment.authz_id, Permission::Update)
        .unwrap());

    // The creator and admins keep full control over the default environment.
    let admins = store.find_group(&org.id, "admins").unwrap().unwrap();
    let admin = store
        .create_user("admin", "admin@example.com", "bcrypt", "h", "s", false)
        .unwrap();
    store
        .add_group_member(&admins.authz_id, &admin.authz_id)
        .unwrap();
    for actor in [&creator.authz_id, &admin.authz_id] {
        for permission in Permission::ALL {
            assert!(
                store
                    .check_permission(actor, &environment.authz_id, permission)
                    .unwrap(),
                "missing {permission:?} for {actor}"
            );
        }
    }
}

#[test]
fn roles_created_after_the_policy_inherit_the_roles_container_acl() {
    let store = SqliteStore::open_in_memory().unwrap();
    let creator = store
        .create_user("root", "root@example.com", "bcrypt", "hash", "salt", true)
        .unwrap();
    let org =
        create_organization(&store, "acme", "Acme Corp", &creator.id, &creator.authz_id).unwrap();

    let role = store
        .create_role(
            &org.id,
            "web",
            r#"{"name":"web","run_list":[]}"#,
            &creator.authz_id,
        )
        .unwrap();
    let clients = store.find_group(&org.id, "clients").unwrap().unwrap();
    let client = store.create_client(&org.id, "node1", false, false).unwrap();
    store
        .add_group_member(&clients.authz_id, &client.authz_id)
        .unwrap();

    assert!(store
        .check_permission(&client.authz_id, &role.authz_id, Permission::Read)
        .unwrap());
    assert!(!store
        .check_permission(&client.authz_id, &role.authz_id, Permission::Update)
        .unwrap());
    for permission in Permission::ALL {
        assert!(store
            .check_permission(&creator.authz_id, &role.authz_id, permission)
            .unwrap());
    }
}
