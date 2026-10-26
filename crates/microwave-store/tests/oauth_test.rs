use microwave_store::SqliteStore;

fn store_with_user_and_application() -> (SqliteStore, String, i64) {
    let store = SqliteStore::open_in_memory().unwrap();
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
    let application = store
        .create_oauth_application(
            "Chef Supermarket",
            "https://supermarket.example/auth/chef/callback",
            "read",
            true,
        )
        .unwrap();
    (store, user.id, application.id)
}

#[test]
fn creates_lists_updates_and_deletes_oauth_applications() {
    let store = SqliteStore::open_in_memory().unwrap();
    let application = store
        .create_oauth_application(
            "Chef Supermarket",
            "https://supermarket.example/auth/chef/callback",
            "read",
            true,
        )
        .unwrap();

    assert_eq!(application.name, "Chef Supermarket");
    assert!(!application.uid.is_empty());
    assert!(!application.secret.is_empty());
    assert!(application.confidential);

    assert_eq!(
        store
            .find_oauth_application(application.id)
            .unwrap()
            .unwrap(),
        application
    );
    assert_eq!(
        store
            .find_oauth_application_by_uid(&application.uid)
            .unwrap()
            .unwrap(),
        application
    );
    assert_eq!(store.list_oauth_applications().unwrap().len(), 1);

    let updated = store
        .update_oauth_application(
            application.id,
            Some("Renamed"),
            Some("https://example.test/callback"),
        )
        .unwrap();
    assert_eq!(updated.name, "Renamed");
    assert_eq!(updated.redirect_uri, "https://example.test/callback");
    assert_eq!(updated.uid, application.uid);
    assert_eq!(updated.secret, application.secret);

    store.delete_oauth_application(application.id).unwrap();
    assert!(store
        .find_oauth_application(application.id)
        .unwrap()
        .is_none());
}

#[test]
fn issues_and_revokes_access_grants() {
    let (store, user_id, application_id) = store_with_user_and_application();

    let grant = store
        .create_oauth_access_grant(
            &user_id,
            application_id,
            "https://supermarket.example/auth/chef/callback",
            "read",
            600,
        )
        .unwrap();
    assert_eq!(grant.resource_owner_id, user_id);
    assert_eq!(grant.application_id, application_id);
    assert_eq!(grant.expires_in, 600);
    assert!(grant.revoked_at.is_none());
    assert_eq!(
        store
            .find_oauth_access_grant(&grant.token)
            .unwrap()
            .unwrap(),
        grant
    );

    store.revoke_oauth_access_grant(&grant.token).unwrap();
    let revoked = store
        .find_oauth_access_grant(&grant.token)
        .unwrap()
        .unwrap();
    assert!(revoked.revoked_at.is_some());
}

#[test]
fn issues_access_and_refresh_tokens() {
    let (store, user_id, application_id) = store_with_user_and_application();

    let token = store
        .create_oauth_access_token(&user_id, application_id, "read", Some(7200), true)
        .unwrap();
    assert_eq!(token.resource_owner_id.as_deref(), Some(user_id.as_str()));
    assert_eq!(token.application_id, Some(application_id));
    assert_eq!(token.expires_in, Some(7200));
    assert_eq!(token.scopes, "read");
    assert!(token.revoked_at.is_none());
    let refresh_token = token.refresh_token.clone().unwrap();
    assert!(!refresh_token.is_empty());

    assert_eq!(
        store
            .find_oauth_access_token(&token.token)
            .unwrap()
            .unwrap(),
        token
    );
    assert_eq!(
        store
            .find_oauth_access_token_by_refresh(&refresh_token)
            .unwrap()
            .unwrap(),
        token
    );

    store.revoke_oauth_access_token(&token.token).unwrap();
    assert!(store
        .find_oauth_access_token(&token.token)
        .unwrap()
        .unwrap()
        .revoked_at
        .is_some());

    let second = store
        .create_oauth_access_token(&user_id, application_id, "read", Some(7200), false)
        .unwrap();
    assert!(second.refresh_token.is_none());
    store
        .revoke_oauth_access_token_by_refresh(&refresh_token)
        .unwrap();
    assert!(store
        .find_oauth_access_token_by_refresh(&refresh_token)
        .unwrap()
        .unwrap()
        .revoked_at
        .is_some());
}

#[test]
fn deleting_an_application_removes_its_tokens_and_grants() {
    let (store, user_id, application_id) = store_with_user_and_application();
    let grant = store
        .create_oauth_access_grant(
            &user_id,
            application_id,
            "https://example.test",
            "read",
            600,
        )
        .unwrap();
    let token = store
        .create_oauth_access_token(&user_id, application_id, "read", Some(7200), true)
        .unwrap();

    store.delete_oauth_application(application_id).unwrap();

    assert!(store
        .find_oauth_access_grant(&grant.token)
        .unwrap()
        .is_none());
    assert!(store
        .find_oauth_access_token(&token.token)
        .unwrap()
        .is_none());
}

#[test]
fn stores_and_deletes_id_sessions() {
    let store = SqliteStore::open_in_memory().unwrap();
    let token = store.create_id_session("alice").unwrap();
    assert!(!token.is_empty());
    assert_eq!(
        store.find_id_session(&token).unwrap().as_deref(),
        Some("alice")
    );

    store.delete_id_session(&token).unwrap();
    assert!(store.find_id_session(&token).unwrap().is_none());
}

#[test]
fn consumes_password_reset_tokens_once() {
    let store = SqliteStore::open_in_memory().unwrap();
    let token = store.create_password_reset_token("alice").unwrap();

    assert_eq!(
        store
            .consume_password_reset_token(&token, 3600)
            .unwrap()
            .as_deref(),
        Some("alice")
    );
    assert!(store
        .consume_password_reset_token(&token, 3600)
        .unwrap()
        .is_none());
}

#[test]
fn password_reset_tokens_expire() {
    let store = SqliteStore::open_in_memory().unwrap();
    let token = store.create_password_reset_token("alice").unwrap();

    assert_eq!(
        store.find_password_reset_token(&token, 3600).unwrap(),
        Some("alice".into())
    );
    assert_eq!(store.find_password_reset_token(&token, 0).unwrap(), None);
    assert_eq!(
        store.consume_password_reset_token(&token, 3600).unwrap(),
        None,
        "expired token must not be consumable"
    );
}
