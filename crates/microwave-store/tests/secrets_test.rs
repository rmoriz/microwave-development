use microwave_store::{secret_kv_key, SqliteStore, StoreError};

#[test]
fn kv_values_round_trip_and_overwrite() {
    let store = SqliteStore::open_in_memory().unwrap();

    assert_eq!(store.get_kv("sample").unwrap(), None);
    store.set_kv("sample", "one").unwrap();
    assert_eq!(store.get_kv("sample").unwrap().as_deref(), Some("one"));

    store.set_kv("sample", "two").unwrap();
    assert_eq!(store.get_kv("sample").unwrap().as_deref(), Some("two"));

    assert!(store.remove_kv("sample").unwrap());
    assert_eq!(store.get_kv("sample").unwrap(), None);
    assert!(!store.remove_kv("sample").unwrap());
}

#[test]
fn secrets_are_namespaced_by_group_and_name() {
    let store = SqliteStore::open_in_memory().unwrap();

    store
        .set_secret("object_store", "secret", "bookshelf-hmac")
        .unwrap();
    store
        .set_secret("bookshelf", "secret", "other-value")
        .unwrap();

    assert_eq!(
        store
            .get_secret("object_store", "secret")
            .unwrap()
            .as_deref(),
        Some("bookshelf-hmac")
    );
    assert_eq!(
        store.get_secret("bookshelf", "secret").unwrap().as_deref(),
        Some("other-value")
    );
    assert_eq!(store.get_secret("object_store", "missing").unwrap(), None);

    // The raw kvpair key is prefixed so it cannot collide with session data.
    assert_eq!(
        store
            .get_kv(&secret_kv_key("object_store", "secret"))
            .unwrap()
            .as_deref(),
        Some("bookshelf-hmac")
    );
}

#[test]
fn setting_a_secret_requires_a_value_and_identifiers() {
    let store = SqliteStore::open_in_memory().unwrap();

    assert!(matches!(
        store.set_secret("object_store", "secret", ""),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.set_secret("", "secret", "value"),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.set_secret("object_store", "", "value"),
        Err(StoreError::Invalid(_))
    ));
    assert!(matches!(
        store.set_secret("object_store", "bad:name", "value"),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn removing_a_missing_secret_reports_not_found() {
    let store = SqliteStore::open_in_memory().unwrap();

    assert!(matches!(
        store.remove_secret("object_store", "secret"),
        Err(StoreError::NotFound(_))
    ));

    store.set_secret("object_store", "secret", "value").unwrap();
    store.remove_secret("object_store", "secret").unwrap();
    assert_eq!(store.get_secret("object_store", "secret").unwrap(), None);
}

#[test]
fn setting_a_secret_if_absent_keeps_the_first_value() {
    let store = SqliteStore::open_in_memory().unwrap();

    assert_eq!(
        store
            .set_secret_if_absent("object_store", "secret", "first")
            .unwrap(),
        "first"
    );
    assert_eq!(
        store
            .set_secret_if_absent("object_store", "secret", "second")
            .unwrap(),
        "first"
    );
    assert_eq!(
        store
            .get_secret("object_store", "secret")
            .unwrap()
            .as_deref(),
        Some("first")
    );
    assert!(matches!(
        store.set_secret_if_absent("object_store", "secret", ""),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn maintenance_defaults_to_disabled_and_persists() {
    let store = SqliteStore::open_in_memory().unwrap();

    assert!(!store.maintenance_enabled().unwrap());
    store.set_maintenance_enabled(true).unwrap();
    assert!(store.maintenance_enabled().unwrap());
    store.set_maintenance_enabled(false).unwrap();
    assert!(!store.maintenance_enabled().unwrap());
}
