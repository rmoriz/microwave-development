use microwave_store::{SqliteStore, UserPatch};

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

#[test]
fn adds_user_profile_columns_to_legacy_databases() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE users (
                     id TEXT PRIMARY KEY,
                     authz_id TEXT NOT NULL UNIQUE,
                     username TEXT NOT NULL UNIQUE,
                     email TEXT UNIQUE,
                     password_hash TEXT NOT NULL DEFAULT '',
                     password_salt TEXT NOT NULL DEFAULT '',
                     hash_type TEXT NOT NULL DEFAULT 'bcrypt',
                     admin INTEGER NOT NULL DEFAULT 0,
                     recovery_authentication_enabled INTEGER NOT NULL DEFAULT 0,
                     external_authentication_uid TEXT,
                     last_updated_by TEXT NOT NULL DEFAULT '',
                     created_at TEXT NOT NULL,
                     updated_at TEXT NOT NULL
                 );
                 INSERT INTO users
                     (id, authz_id, username, email, created_at, updated_at)
                 VALUES
                     ('u1', 'a1', 'alice', 'alice@example.com',
                      '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');",
            )
            .unwrap();
    }

    let store = SqliteStore::open(&path).unwrap();
    let user = store.find_user("alice").unwrap().unwrap();
    assert_eq!(user.display_name, None);
    assert_eq!(user.first_name, None);
    store
        .update_user_patch(
            "alice",
            &UserPatch {
                display_name: Some(Some("Alice".into())),
                first_name: Some(Some("Alice".into())),
                ..Default::default()
            },
        )
        .unwrap();
    let user = store.find_user("alice").unwrap().unwrap();
    assert_eq!(user.display_name.as_deref(), Some("Alice"));
    assert_eq!(user.first_name.as_deref(), Some("Alice"));
}
