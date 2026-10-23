use microwave_store::{ActorType, SqliteStore};

#[test]
fn finds_user_and_client_with_the_same_name() {
    let store = SqliteStore::open_in_memory().unwrap();
    let org = store.create_org("acme", "Acme").unwrap();
    let user = store
        .create_user("web1", "web1@example.com", "bcrypt", "h", "s", false)
        .unwrap();
    let client = store.create_client(&org.id, "web1", false, false).unwrap();
    store
        .add_actor_key(&user.id, ActorType::User, "default", "user-key")
        .unwrap();
    store
        .add_actor_key(&client.id, ActorType::Client, "default", "client-key")
        .unwrap();

    let principals = store.find_principals(Some(&org.id), "web1").unwrap();
    assert_eq!(principals.len(), 2);
    assert!(principals.iter().any(|p| p.actor_type == ActorType::User));
    assert!(principals.iter().any(|p| p.actor_type == ActorType::Client));

    let client_principal = principals
        .iter()
        .find(|p| p.actor_type == ActorType::Client)
        .unwrap();
    assert_eq!(
        store.find_principal_keys(client_principal).unwrap()[0].public_key,
        "client-key"
    );
}
