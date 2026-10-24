use microwave_store::{ActorType, SqliteStore, UserPatch, UserProfile};

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

#[test]
fn stores_user_profile_and_patches_it() {
    let store = SqliteStore::open_in_memory().unwrap();
    let profile = UserProfile {
        first_name: Some("Alice".into()),
        middle_name: Some("".into()),
        last_name: Some("Smith".into()),
        display_name: Some("Alice Smith".into()),
        ..Default::default()
    };
    let user = store
        .create_user_with_profile(
            "alice",
            Some("Alice@Example.COM"),
            "bcrypt",
            "hash",
            "salt",
            false,
            &profile,
        )
        .unwrap();
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
    assert_eq!(user.first_name.as_deref(), Some("Alice"));
    assert_eq!(user.middle_name.as_deref(), Some(""));
    assert_eq!(user.last_name.as_deref(), Some("Smith"));
    assert_eq!(user.display_name.as_deref(), Some("Alice Smith"));

    let updated = store
        .update_user_patch(
            "alice",
            &UserPatch {
                new_username: Some("alice2".into()),
                email: Some("new@example.com".into()),
                display_name: Some(Some("Alice Jones".into())),
                first_name: Some(None),
                external_authentication_uid: Some(Some("uid-1".into())),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(updated.username, "alice2");
    assert_eq!(updated.email.as_deref(), Some("new@example.com"));
    assert_eq!(updated.display_name.as_deref(), Some("Alice Jones"));
    assert_eq!(updated.first_name, None);
    assert_eq!(updated.last_name.as_deref(), Some("Smith"));
    assert_eq!(
        updated.external_authentication_uid.as_deref(),
        Some("uid-1")
    );
    assert!(store.find_user("alice").unwrap().is_none());
    assert!(store.find_user("alice2").unwrap().is_some());
}

#[test]
fn user_email_conflicts_are_case_insensitive() {
    let store = SqliteStore::open_in_memory().unwrap();
    let profile = UserProfile::default();
    store
        .create_user_with_profile(
            "alice",
            Some("User@aol.com"),
            "bcrypt",
            "h",
            "s",
            false,
            &profile,
        )
        .unwrap();
    let error = store
        .create_user_with_profile(
            "bob",
            Some("user@AOL.COM"),
            "bcrypt",
            "h",
            "s",
            false,
            &profile,
        )
        .unwrap_err();
    assert!(matches!(error, microwave_store::StoreError::Conflict(_)));
}

#[test]
fn filters_users_by_email_and_external_uid() {
    let store = SqliteStore::open_in_memory().unwrap();
    store
        .create_user("alice", "User@aol.com", "bcrypt", "h", "s", false)
        .unwrap();
    let saml = store
        .create_user("saml", "saml@example.com", "bcrypt", "h", "s", false)
        .unwrap();
    store
        .update_user_patch(
            "saml",
            &UserPatch {
                external_authentication_uid: Some(Some("sam.l.jackson".into())),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        store
            .list_users_filtered(Some("user@aol.com"), None)
            .unwrap()
            .iter()
            .map(|user| user.username.as_str())
            .collect::<Vec<_>>(),
        vec!["alice"]
    );
    assert_eq!(
        store
            .list_users_filtered(None, Some("sam.l.jackson"))
            .unwrap()
            .iter()
            .map(|user| user.username.as_str())
            .collect::<Vec<_>>(),
        vec!["saml"]
    );
    assert!(!saml.admin);
}
