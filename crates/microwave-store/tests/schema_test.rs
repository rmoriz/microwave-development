use microwave_store::SqliteStore;

#[test]
fn opens_in_memory_and_creates_core_tables() {
    let store = SqliteStore::open_in_memory().unwrap();
    let names: Vec<String> = store
        .table_names()
        .unwrap()
        .into_iter()
        .filter(|n| !n.starts_with("sqlite_"))
        .collect();
    for table in [
        "orgs",
        "users",
        "clients",
        "actor_keys",
        "authz_entities",
        "nodes",
        "cookbooks",
    ] {
        assert!(names.contains(&table.to_string()), "missing {table}");
    }
}

#[test]
fn enables_wal_and_foreign_keys_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteStore::open(&dir.path().join("mw.db")).unwrap();
    assert_eq!(store.journal_mode().unwrap(), "wal");
    assert!(store.foreign_keys_enabled().unwrap());
}
