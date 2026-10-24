use microwave_store::SqliteStore;

#[test]
fn org_membership_can_be_listed_and_removed() {
    let store = SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("acme", "Acme Corp").unwrap();
    let user = store
        .create_user(
            "alice",
            "alice@example.com",
            "bcrypt",
            "hash",
            "salt",
            false,
        )
        .unwrap();

    store.associate_user_org(&org.id, &user.id).unwrap();
    let members = store.list_org_users(&org.id).unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].username, "alice");

    store
        .remove_user_from_org(&org.id, &user.id, &user.authz_id)
        .unwrap();
    assert!(store.list_org_users(&org.id).unwrap().is_empty());
    assert!(store
        .remove_user_from_org(&org.id, &user.id, &user.authz_id)
        .is_err());
}

#[test]
fn removing_an_org_member_also_removes_org_group_membership() {
    let store = SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("acme", "Acme Corp").unwrap();
    let group = store.create_group_for_org(&org.id, "users").unwrap();
    let user = store
        .create_user(
            "alice",
            "alice@example.com",
            "bcrypt",
            "hash",
            "salt",
            false,
        )
        .unwrap();
    store.associate_user_org(&org.id, &user.id).unwrap();
    store
        .add_group_member(&group.authz_id, &user.authz_id)
        .unwrap();

    store
        .remove_user_from_org(&org.id, &user.id, &user.authz_id)
        .unwrap();

    // The users group no longer grants the actor anything in this org.
    let target = store.create_object().unwrap();
    store
        .set_acl(
            &target,
            microwave_store::Permission::Read,
            None,
            Some(&group.authz_id),
        )
        .unwrap();
    assert!(!store
        .check_permission(&user.authz_id, &target, microwave_store::Permission::Read)
        .unwrap());
}
