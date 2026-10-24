use microwave_store::{Org, SqliteStore, StoreError, User};

fn user(store: &SqliteStore, name: &str) -> User {
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
}

struct Fixture {
    store: SqliteStore,
    org: Org,
    users_group: String,
    admins_group: String,
}

fn fixture() -> Fixture {
    let store = SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("acme", "Acme Corp").unwrap();
    let users_group = store
        .create_group_for_org(&org.id, "users")
        .unwrap()
        .authz_id;
    let admins_group = store
        .create_group_for_org(&org.id, "admins")
        .unwrap()
        .authz_id;
    Fixture {
        store,
        org,
        users_group,
        admins_group,
    }
}

#[test]
fn associates_and_dissociates_users_with_the_org_users_group() {
    let fixture = fixture();
    let store = &fixture.store;
    let alice = user(store, "alice");

    store.add_user_to_org(&fixture.org.id, &alice.id).unwrap();

    assert!(store.is_user_in_org(&fixture.org.id, &alice.id).unwrap());
    assert_eq!(
        store
            .list_org_users(&fixture.org.id)
            .unwrap()
            .iter()
            .map(|user| user.username.as_str())
            .collect::<Vec<_>>(),
        vec!["alice"]
    );
    assert!(store
        .user_in_group(&fixture.users_group, &alice.authz_id)
        .unwrap());
    assert_eq!(
        store
            .list_user_orgs(&alice.id)
            .unwrap()
            .iter()
            .map(|org| org.name.as_str())
            .collect::<Vec<_>>(),
        vec!["acme"]
    );

    store
        .remove_user_from_org(&fixture.org.id, &alice.id, "alice")
        .unwrap();

    assert!(!store.is_user_in_org(&fixture.org.id, &alice.id).unwrap());
    assert!(store.list_org_users(&fixture.org.id).unwrap().is_empty());
    assert!(!store
        .user_in_group(&fixture.users_group, &alice.authz_id)
        .unwrap());
    assert!(store.list_user_orgs(&alice.id).unwrap().is_empty());
}

#[test]
fn refuses_to_remove_a_user_who_is_still_an_admin() {
    let fixture = fixture();
    let store = &fixture.store;
    let alice = user(store, "alice");
    store.add_user_to_org(&fixture.org.id, &alice.id).unwrap();
    store
        .add_group_member(&fixture.admins_group, &alice.authz_id)
        .unwrap();

    let error = store
        .remove_user_from_org(&fixture.org.id, &alice.id, "alice")
        .unwrap_err();
    match error {
        StoreError::Forbidden(message) => assert_eq!(
            message,
            "Please remove alice from this organization's admins group before removing him or her from the organization."
        ),
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(store.is_user_in_org(&fixture.org.id, &alice.id).unwrap());

    store
        .remove_group_member(&fixture.admins_group, &alice.authz_id)
        .unwrap();
    store
        .remove_user_from_org(&fixture.org.id, &alice.id, "alice")
        .unwrap();
    assert!(!store.is_user_in_org(&fixture.org.id, &alice.id).unwrap());
}

#[test]
fn creates_lists_and_accepts_invitations() {
    let fixture = fixture();
    let store = &fixture.store;
    let bob = user(store, "bob");

    let invitation = store
        .create_invitation(&fixture.org.id, &bob.id, "inviter")
        .unwrap();
    assert_eq!(invitation.org_name, "acme");
    assert_eq!(invitation.username, "bob");

    let org_invitations = store.list_org_invitations(&fixture.org.id).unwrap();
    assert_eq!(org_invitations, vec![invitation.clone()]);
    assert_eq!(
        store.list_user_invitations(&bob.id).unwrap(),
        vec![invitation.clone()]
    );
    assert_eq!(store.count_user_invitations(&bob.id).unwrap(), 1);
    assert_eq!(
        store.find_invitation(&invitation.id).unwrap(),
        Some(invitation.clone())
    );

    store.accept_invitation(&invitation.id).unwrap();

    assert!(store.is_user_in_org(&fixture.org.id, &bob.id).unwrap());
    assert!(store
        .user_in_group(&fixture.users_group, &bob.authz_id)
        .unwrap());
    assert_eq!(store.count_user_invitations(&bob.id).unwrap(), 0);
    assert_eq!(store.find_invitation(&invitation.id).unwrap(), None);
}

#[test]
fn rejects_invitations_without_associating_the_user() {
    let fixture = fixture();
    let store = &fixture.store;
    let carol = user(store, "carol");

    let invitation = store
        .create_invitation(&fixture.org.id, &carol.id, "inviter")
        .unwrap();
    store.reject_invitation(&invitation.id).unwrap();

    assert!(!store.is_user_in_org(&fixture.org.id, &carol.id).unwrap());
    assert_eq!(store.count_user_invitations(&carol.id).unwrap(), 0);
    assert_eq!(store.find_invitation(&invitation.id).unwrap(), None);
}

#[test]
fn refuses_duplicate_invitations_and_associations() {
    let fixture = fixture();
    let store = &fixture.store;
    let dave = user(store, "dave");

    store
        .create_invitation(&fixture.org.id, &dave.id, "inviter")
        .unwrap();
    let error = store
        .create_invitation(&fixture.org.id, &dave.id, "inviter")
        .unwrap_err();
    match error {
        StoreError::Conflict(message) => assert_eq!(message, "The invite already exists."),
        other => panic!("unexpected error: {other:?}"),
    }

    store.add_user_to_org(&fixture.org.id, &dave.id).unwrap();
    let error = store
        .add_user_to_org(&fixture.org.id, &dave.id)
        .unwrap_err();
    match error {
        StoreError::Conflict(message) => assert_eq!(message, "The association already exists."),
        other => panic!("unexpected error: {other:?}"),
    }
}
