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
fn creates_authz_and_performance_indexes_and_constraints() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("indexes.db");
    {
        let _store = SqliteStore::open(&path).unwrap();
    }

    let connection = rusqlite::Connection::open(&path).unwrap();
    let indexes: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'index'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for index in [
        "users_lower_email_index",
        "users_external_authentication_uid_index",
        "authz_entities_org_index",
        "authz_acl_actor_actor_index",
        "authz_group_actors_actor_index",
        "sandboxed_checksums_sandbox_index",
        "cookbook_versions_org_name_version_index",
        "cookbook_artifact_versions_org_artifact_identifier_index",
        "cookbook_artifact_version_checksums_version_index",
    ] {
        assert!(indexes.iter().any(|name| name == index), "missing {index}");
    }

    let artifact_fks: Vec<(String, String, String)> = connection
        .prepare("PRAGMA foreign_key_list(cookbook_artifact_versions)")
        .unwrap()
        .query_map([], |row| Ok((row.get(3)?, row.get(2)?, row.get(6)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(artifact_fks
        .iter()
        .any(|(from, table, on_delete)| from == "cookbook_artifact_id"
            && table == "cookbook_artifacts"
            && on_delete == "RESTRICT"));

    let association_fks: Vec<(String, String)> = connection
        .prepare("PRAGMA foreign_key_list(policy_group_revision_associations)")
        .unwrap()
        .query_map([], |row| Ok((row.get(2)?, row.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(association_fks
        .iter()
        .any(|(table, from)| table == "policy_groups" && from == "policy_group_authz_id"));
    assert!(association_fks
        .iter()
        .any(|(table, from)| table == "policy_revisions" && from == "policy_revision_name"));
}

#[test]
fn rejects_authz_relation_rows_with_the_wrong_entity_type() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("entity-types.db");
    {
        let _store = SqliteStore::open(&path).unwrap();
    }
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             INSERT INTO authz_entities VALUES
                 ('a1', 'actor', NULL, '2026-01-01T00:00:00Z'),
                 ('a2', 'actor', NULL, '2026-01-01T00:00:00Z'),
                 ('g1', 'group', NULL, '2026-01-01T00:00:00Z'),
                 ('g2', 'group', NULL, '2026-01-01T00:00:00Z'),
                 ('o1', 'object', NULL, '2026-01-01T00:00:00Z');",
        )
        .unwrap();

    for statement in [
        "INSERT INTO authz_group_actors VALUES ('g1', 'g2')",
        "INSERT INTO authz_group_groups VALUES ('a1', 'g1')",
        "INSERT INTO authz_group_groups VALUES ('g1', 'a1')",
        "INSERT INTO authz_acl_actor VALUES ('o1', 'read', 'g1')",
        "INSERT INTO authz_acl_group VALUES ('o1', 'read', 'a1')",
        "INSERT INTO authz_acl_actor VALUES ('missing', 'read', 'a1')",
    ] {
        assert!(
            connection.execute_batch(statement).is_err(),
            "statement should have been rejected: {statement}"
        );
    }

    connection
        .execute_batch(
            "INSERT INTO authz_group_actors VALUES ('g1', 'a1');
             INSERT INTO authz_group_groups VALUES ('g1', 'g2');
             INSERT INTO authz_acl_actor VALUES ('o1', 'read', 'a1');
             INSERT INTO authz_acl_group VALUES ('o1', 'read', 'g1');",
        )
        .unwrap();
}

#[test]
fn enforces_policy_association_and_artifact_parent_constraints() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("constraints.db");
    {
        let _store = SqliteStore::open(&path).unwrap();
    }
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             INSERT INTO orgs
                 (id, authz_id, name, full_name, assigned_at, last_updated_by, created_at, updated_at)
             VALUES
                 ('o1', 'oa1', 'acme', 'Acme', 't', '', 't', 't');
             INSERT INTO authz_entities VALUES
                 ('pa1', 'object', 'o1', 't'),
                 ('ga1', 'object', 'o1', 't'),
                 ('aa1', 'object', 'o1', 't');
             INSERT INTO policies
                 (id, org_id, name, authz_id, last_updated_by, created_at, updated_at)
             VALUES
                 ('p1', 'o1', 'base', 'pa1', '', 't', 't');
             INSERT INTO policy_revisions
                 (id, org_id, policy_id, revision_id, name, serialized_object,
                  last_updated_by, created_at, updated_at)
             VALUES
                 ('pr1', 'o1', 'p1', '1.0.0', 'base', NULL, '', 't', 't');
             INSERT INTO policy_groups
                 (id, org_id, name, authz_id, serialized_object,
                  last_updated_by, created_at, updated_at)
             VALUES
                 ('pg1', 'o1', 'prod', 'ga1', NULL, '', 't', 't');
             INSERT INTO cookbook_artifacts (id, org_id, name, authz_id)
             VALUES (1, 'o1', 'artifact', 'aa1');
             INSERT INTO cookbook_artifact_versions
                 (cookbook_artifact_id, org_id, identifier, metadata,
                  serialized_object, created_at, created_by)
             VALUES (1, 'o1', '1.0.0', NULL, NULL, 't', '');",
        )
        .unwrap();

    let wrong_group = "INSERT INTO policy_group_revision_associations
         (id, org_id, policy_revision_id, policy_group_authz_id,
          policy_revision_name, policy_revision_revision_id, policy_group_name,
          last_updated_by, created_at, updated_at)
         VALUES ('bad1', 'o1', 'pr1', 'ga1', 'base', '1.0.0', 'missing', '', 't', 't')";
    assert!(connection.execute_batch(wrong_group).is_err());

    let wrong_revision = "INSERT INTO policy_group_revision_associations
         (id, org_id, policy_revision_id, policy_group_authz_id,
          policy_revision_name, policy_revision_revision_id, policy_group_name,
          last_updated_by, created_at, updated_at)
         VALUES ('bad2', 'o1', 'pr1', 'ga1', 'missing', '9.9.9', 'prod', '', 't', 't')";
    assert!(connection.execute_batch(wrong_revision).is_err());

    connection
        .execute_batch(
            "INSERT INTO policy_group_revision_associations
                 (id, org_id, policy_revision_id, policy_group_authz_id,
                  policy_revision_name, policy_revision_revision_id, policy_group_name,
                  last_updated_by, created_at, updated_at)
             VALUES ('ok1', 'o1', 'pr1', 'ga1', 'base', '1.0.0', 'prod', '', 't', 't');",
        )
        .unwrap();

    assert!(connection
        .execute_batch("DELETE FROM cookbook_artifacts WHERE id = 1")
        .is_err());
    connection
        .execute_batch(
            "DELETE FROM cookbook_artifact_versions WHERE cookbook_artifact_id = 1;
             DELETE FROM cookbook_artifacts WHERE id = 1;",
        )
        .unwrap();
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
