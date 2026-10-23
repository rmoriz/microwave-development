use microwave_authz::{check, Permission};
use microwave_store::SqliteStore;

#[test]
fn direct_and_transitive_grants_are_seen() {
    let store = SqliteStore::open_in_memory().unwrap();
    let actor = store.create_actor().unwrap();
    let target = store.create_object().unwrap();
    let child = store.create_group("readers").unwrap();
    let parent = store.create_group("admins").unwrap();

    store.add_group_member(&child, &actor).unwrap();
    store.add_group_child(&parent, &child).unwrap();
    store
        .set_acl(&target, Permission::Read, Some(&actor), None)
        .unwrap();

    assert!(check(&store, &actor, &target, Permission::Read).unwrap());
    assert!(!check(&store, &actor, &target, Permission::Update).unwrap());

    let shared = store.create_object().unwrap();
    store
        .set_acl(&shared, Permission::Read, None, Some(&parent))
        .unwrap();
    assert!(check(&store, &actor, &shared, Permission::Read).unwrap());
}

#[test]
fn group_cycles_are_rejected() {
    let store = SqliteStore::open_in_memory().unwrap();
    let a = store.create_group("a").unwrap();
    let b = store.create_group("b").unwrap();
    store.add_group_child(&a, &b).unwrap();
    assert!(store.add_group_child(&b, &a).is_err());
}

#[test]
fn unknown_target_denies_without_panicking() {
    let store = SqliteStore::open_in_memory().unwrap();
    let actor = store.create_actor().unwrap();
    assert!(!check(&store, &actor, "does-not-exist", Permission::Read).unwrap());
}
