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
