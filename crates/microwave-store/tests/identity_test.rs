use microwave_store::{ActorType, SqliteStore};

#[test]
fn creates_and_finds_org_user_client_and_keys() {
    let store = SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("acme", "Acme Corp").unwrap();
    assert_eq!(store.find_org("acme").unwrap().unwrap().id, org.id);

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
    assert_eq!(store.find_user("alice").unwrap().unwrap().id, user.id);

    let client = store.create_client(&org.id, "web1", false, false).unwrap();
    assert_eq!(
        store.find_client(&org.id, "web1").unwrap().unwrap().id,
        client.id
    );

    store
        .add_actor_key(
            &user.id,
            ActorType::User,
            "default",
            "-----BEGIN PUBLIC KEY-----\nx\n-----END PUBLIC KEY-----",
        )
        .unwrap();
    assert_eq!(
        store
            .find_actor_keys(&user.id, ActorType::User)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn duplicate_names_conflict() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.create_org("acme", "Acme Corp").unwrap();
    assert!(store.create_org("acme", "Other").is_err());
}
