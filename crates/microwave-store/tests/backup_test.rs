use microwave_store::SqliteStore;

#[test]
fn backs_up_a_live_database_to_a_verified_copy() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("microwave.db")).unwrap();
    store
        .create_user(
            "alice",
            "alice@example.com",
            "bcrypt",
            "hash",
            "salt",
            false,
        )
        .unwrap();

    let backup = dir.path().join("backup.db");
    store.backup_to(&backup).unwrap();
    SqliteStore::verify_database(&backup).unwrap();

    let restored = SqliteStore::open(&backup).unwrap();
    assert_eq!(
        restored.find_user("alice").unwrap().unwrap().username,
        "alice"
    );
}

#[test]
fn rejects_a_file_that_is_not_a_microwave_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("not-a-database");
    std::fs::write(&path, "hello").unwrap();
    assert!(SqliteStore::verify_database(&path).is_err());
}
