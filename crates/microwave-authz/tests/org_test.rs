use microwave_authz::{create_organization, Permission};
use microwave_store::{SqliteStore, StoreError};

#[tokio::test]
async fn creates_default_containers_groups_and_acls() {
    let store = SqliteStore::open_in_memory().unwrap();
    let creator = store
        .create_user("root", "root@example.com", "bcrypt", "hash", "salt", true)
        .unwrap();
    let org = create_organization(&store, "acme", "Acme Corp", &creator.id, &creator.authz_id)
        .await
        .unwrap();

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

#[tokio::test]
async fn default_environment_inherits_the_environments_container_acl() {
    let store = SqliteStore::open_in_memory().unwrap();
    let creator = store
        .create_user("root", "root@example.com", "bcrypt", "hash", "salt", true)
        .unwrap();
    let org = create_organization(&store, "acme", "Acme Corp", &creator.id, &creator.authz_id)
        .await
        .unwrap();

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

#[tokio::test]
async fn roles_created_after_the_policy_inherit_the_roles_container_acl() {
    let store = SqliteStore::open_in_memory().unwrap();
    let creator = store
        .create_user("root", "root@example.com", "bcrypt", "hash", "salt", true)
        .unwrap();
    let org = create_organization(&store, "acme", "Acme Corp", &creator.id, &creator.authz_id)
        .await
        .unwrap();

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

fn acl_groups(store: &SqliteStore, authz_id: &str, permission: Permission) -> Vec<String> {
    store
        .get_acl_entries(authz_id)
        .unwrap()
        .into_iter()
        .find(|entry| entry.permission == permission)
        .unwrap()
        .groups
}

fn acl_users(store: &SqliteStore, authz_id: &str, permission: Permission) -> Vec<String> {
    store
        .get_acl_entries(authz_id)
        .unwrap()
        .into_iter()
        .find(|entry| entry.permission == permission)
        .unwrap()
        .users
}

#[tokio::test]
async fn applies_the_upstream_default_container_and_group_matrices() {
    let store = SqliteStore::open_in_memory().unwrap();
    let creator = store
        .create_user("root", "root@example.com", "bcrypt", "hash", "salt", true)
        .unwrap();
    let org = create_organization(&store, "acme", "Acme Corp", &creator.id, &creator.authz_id)
        .await
        .unwrap();

    let nodes = store.find_container(&org.id, "nodes").unwrap().unwrap();
    for permission in [
        Permission::Create,
        Permission::Read,
        Permission::Update,
        Permission::Delete,
    ] {
        assert!(acl_users(&store, &nodes.authz_id, permission).contains(&"root".to_string()));
    }
    assert_eq!(
        acl_groups(&store, &nodes.authz_id, Permission::Create),
        vec!["admins", "clients", "users"]
    );
    assert_eq!(
        acl_groups(&store, &nodes.authz_id, Permission::Update),
        vec!["admins", "users"]
    );

    let clients = store.find_container(&org.id, "clients").unwrap().unwrap();
    assert_eq!(
        acl_groups(&store, &clients.authz_id, Permission::Read),
        vec!["admins", "users"]
    );
    assert_eq!(
        acl_groups(&store, &clients.authz_id, Permission::Delete),
        vec!["admins", "users"]
    );
    assert_eq!(
        acl_groups(&store, &clients.authz_id, Permission::Grant),
        vec!["admins"]
    );

    let admins = store.find_group(&org.id, "admins").unwrap().unwrap();
    let billing = store
        .find_group(&org.id, "billing-admins")
        .unwrap()
        .unwrap();
    let public_key_read = store
        .find_group(&org.id, "public_key_read_access")
        .unwrap()
        .unwrap();
    assert_eq!(
        acl_groups(&store, &admins.authz_id, Permission::Grant),
        vec!["admins"]
    );
    assert_eq!(
        acl_groups(&store, &billing.authz_id, Permission::Read),
        vec!["billing-admins"]
    );
    assert_eq!(
        acl_groups(&store, &billing.authz_id, Permission::Update),
        vec!["billing-admins"]
    );
    assert!(acl_groups(&store, &billing.authz_id, Permission::Create).is_empty());
    assert!(acl_groups(&store, &billing.authz_id, Permission::Delete).is_empty());
    assert!(acl_groups(&store, &billing.authz_id, Permission::Grant).is_empty());
    assert_eq!(
        acl_groups(&store, &public_key_read.authz_id, Permission::Read),
        vec!["admins"]
    );
    assert_eq!(
        acl_groups(&store, &public_key_read.authz_id, Permission::Update),
        vec!["admins"]
    );
    assert!(acl_groups(&store, &public_key_read.authz_id, Permission::Create).is_empty());

    let org_acl = store.get_acl_entries(&org.authz_id).unwrap();
    let org_read = org_acl
        .iter()
        .find(|entry| entry.permission == Permission::Read)
        .unwrap();
    assert_eq!(org_read.groups, vec!["admins", "users"]);
}

#[tokio::test]
async fn creates_global_read_access_and_server_admin_groups() {
    let store = SqliteStore::open_in_memory().unwrap();
    let creator = store
        .create_user("root", "root@example.com", "bcrypt", "hash", "salt", true)
        .unwrap();
    let org = create_organization(&store, "acme", "Acme Corp", &creator.id, &creator.authz_id)
        .await
        .unwrap();

    let read_access = store
        .find_global_group("acme_read_access_group")
        .unwrap()
        .expect("read access group");
    let members = store
        .list_group_members(&org.id, &read_access.authz_id)
        .unwrap();
    assert_eq!(members.groups, vec!["admins", "users"]);

    let server_admins = store
        .find_global_group("server-admins")
        .unwrap()
        .expect("server admins");
    assert!(store
        .user_in_group(&server_admins.authz_id, &creator.authz_id)
        .unwrap());

    let user_acl = store.get_acl_entries(&creator.authz_id).unwrap();
    let read = user_acl
        .iter()
        .find(|entry| entry.permission == Permission::Read)
        .unwrap();
    assert!(read.users.contains(&"root".to_string()));
    assert!(read
        .groups
        .contains(&"::acme_read_access_group".to_string()));
    assert!(read.groups.contains(&"::server-admins".to_string()));
}

#[tokio::test]
async fn deleting_an_organization_removes_its_global_read_access_group_and_acl_rows() {
    let store = SqliteStore::open_in_memory().unwrap();
    let creator = store
        .create_user("root", "root@example.com", "bcrypt", "hash", "salt", true)
        .unwrap();
    let org = create_organization(&store, "acme", "Acme Corp", &creator.id, &creator.authz_id)
        .await
        .unwrap();
    let read_access = store
        .find_global_group("acme_read_access_group")
        .unwrap()
        .expect("read access group");
    assert!(!store
        .get_acl_entries(&read_access.authz_id)
        .unwrap()
        .is_empty());

    store.delete_org(&org.id).unwrap();

    assert!(store.find_org("acme").unwrap().is_none());
    assert!(store
        .find_global_group("acme_read_access_group")
        .unwrap()
        .is_none());
    assert!(!store
        .list_global_groups()
        .unwrap()
        .iter()
        .any(|group| group.name == "acme_read_access_group"));
    match store.get_acl_entries(&read_access.authz_id) {
        Ok(entries) => assert!(entries.is_empty(), "ACL rows survived: {entries:?}"),
        Err(StoreError::NotFound(_)) => {}
        Err(error) => panic!("ACL rows after org deletion: {error}"),
    }
}
