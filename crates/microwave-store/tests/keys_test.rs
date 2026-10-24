use microwave_store::{ActorType, SqliteStore};

fn store_with_user() -> (SqliteStore, String) {
    let store = SqliteStore::open_in_memory().unwrap();
    let user = store
        .create_user("alice", "alice@example.com", "bcrypt", "h", "s", false)
        .unwrap();
    store
        .add_actor_key(&user.id, ActorType::User, "default", "default-key")
        .unwrap();
    (store, user.id)
}

#[test]
fn lists_keys_with_expired_status() {
    let (store, user_id) = store_with_user();
    store
        .create_actor_key(
            &user_id,
            ActorType::User,
            "future",
            "future-key",
            "2050-12-24T21:00:00Z",
        )
        .unwrap();
    store
        .create_actor_key(
            &user_id,
            ActorType::User,
            "past",
            "past-key",
            "2012-12-24T21:00:00Z",
        )
        .unwrap();

    let keys = store.list_actor_keys(&user_id, ActorType::User).unwrap();
    assert_eq!(keys.len(), 3);

    let default = keys.iter().find(|key| key.key_name == "default").unwrap();
    assert!(!default.expired);
    let future = keys.iter().find(|key| key.key_name == "future").unwrap();
    assert!(!future.expired);
    let past = keys.iter().find(|key| key.key_name == "past").unwrap();
    assert!(past.expired);
}

#[test]
fn creates_and_finds_a_key() {
    let (store, user_id) = store_with_user();
    let created = store
        .create_actor_key(&user_id, ActorType::User, "alt", "alt-key", "infinity")
        .unwrap();
    assert_eq!(created.key_name, "alt");
    assert_eq!(created.public_key, "alt-key");
    assert_eq!(created.expires_at, "infinity");
    assert!(!created.expired);

    let found = store
        .find_actor_key(&user_id, ActorType::User, "alt")
        .unwrap()
        .unwrap();
    assert_eq!(found, created);
}

#[test]
fn duplicate_key_names_conflict() {
    let (store, user_id) = store_with_user();
    let error = store
        .create_actor_key(&user_id, ActorType::User, "default", "other", "infinity")
        .unwrap_err();
    assert!(matches!(error, microwave_store::StoreError::Conflict(_)));
}

#[test]
fn updates_key_in_place() {
    let (store, user_id) = store_with_user();
    store
        .create_actor_key(&user_id, ActorType::User, "alt", "alt-key", "infinity")
        .unwrap();

    let updated = store
        .update_actor_key(
            &user_id,
            ActorType::User,
            "alt",
            None,
            Some("new-key"),
            Some("2050-12-24T21:00:00Z"),
        )
        .unwrap();
    assert_eq!(updated.key_name, "alt");
    assert_eq!(updated.public_key, "new-key");
    assert_eq!(updated.expires_at, "2050-12-24T21:00:00Z");
    assert!(!updated.expired);
}

#[test]
fn updates_key_name() {
    let (store, user_id) = store_with_user();
    store
        .create_actor_key(&user_id, ActorType::User, "alt", "alt-key", "infinity")
        .unwrap();

    let updated = store
        .update_actor_key(
            &user_id,
            ActorType::User,
            "alt",
            Some("renamed"),
            None,
            None,
        )
        .unwrap();
    assert_eq!(updated.key_name, "renamed");
    assert!(store
        .find_actor_key(&user_id, ActorType::User, "alt")
        .unwrap()
        .is_none());
    assert!(store
        .find_actor_key(&user_id, ActorType::User, "renamed")
        .unwrap()
        .is_some());
}

#[test]
fn renaming_onto_an_existing_key_conflicts() {
    let (store, user_id) = store_with_user();
    store
        .create_actor_key(&user_id, ActorType::User, "alt", "alt-key", "infinity")
        .unwrap();

    let error = store
        .update_actor_key(
            &user_id,
            ActorType::User,
            "alt",
            Some("default"),
            None,
            None,
        )
        .unwrap_err();
    assert!(matches!(error, microwave_store::StoreError::Conflict(_)));
}

#[test]
fn default_key_cannot_be_renamed() {
    let (store, user_id) = store_with_user();

    let error = store
        .update_actor_key(
            &user_id,
            ActorType::User,
            "default",
            Some("renamed"),
            None,
            None,
        )
        .unwrap_err();
    assert!(matches!(error, microwave_store::StoreError::Invalid(_)));
    assert!(store
        .find_actor_key(&user_id, ActorType::User, "default")
        .unwrap()
        .is_some());
}

#[test]
fn deletes_a_key() {
    let (store, user_id) = store_with_user();
    store
        .create_actor_key(&user_id, ActorType::User, "alt", "alt-key", "infinity")
        .unwrap();

    store
        .delete_actor_key(&user_id, ActorType::User, "alt")
        .unwrap();
    assert!(store
        .find_actor_key(&user_id, ActorType::User, "alt")
        .unwrap()
        .is_none());
    assert!(matches!(
        store
            .delete_actor_key(&user_id, ActorType::User, "alt")
            .unwrap_err(),
        microwave_store::StoreError::NotFound(_)
    ));
}

#[test]
fn missing_key_updates_and_finds_return_not_found() {
    let (store, user_id) = store_with_user();
    assert!(store
        .find_actor_key(&user_id, ActorType::User, "missing")
        .unwrap()
        .is_none());
    assert!(matches!(
        store
            .update_actor_key(
                &user_id,
                ActorType::User,
                "missing",
                Some("new"),
                None,
                None
            )
            .unwrap_err(),
        microwave_store::StoreError::NotFound(_)
    ));
}
