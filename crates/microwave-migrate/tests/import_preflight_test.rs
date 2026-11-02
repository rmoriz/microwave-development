//! Goiardi snapshots land in a fresh target because their import does not
//! merge; Chef API snapshots keep their idempotent upsert behaviour.

use microwave_migrate::{
    import, write_snapshot, MigrateError, Snapshot, SnapshotSource, SnapshotSourceKind,
    SNAPSHOT_FORMAT, SNAPSHOT_VERSION,
};
use microwave_store::{
    AuthzAclActorRow, AuthzEntityRow, AuthzGroupActorRow, GlobalGroupRow, OrgRow, StoreSnapshot,
    UserRow,
};
use std::path::{Path, PathBuf};

fn snapshot(layout: Option<&str>) -> Snapshot {
    Snapshot {
        format: SNAPSHOT_FORMAT.to_string(),
        version: SNAPSHOT_VERSION,
        generated_at: "2026-11-02T00:00:00Z".to_string(),
        source: SnapshotSource {
            kind: SnapshotSourceKind::LegacyApi,
            url: Some("https://chef.example.com".to_string()),
            data_dir: None,
            layout: layout.map(str::to_string),
        },
        secrets_included: false,
        data: StoreSnapshot {
            orgs: vec![OrgRow {
                id: "org-1".to_string(),
                authz_id: "authz-org-1".to_string(),
                name: "org-1".to_string(),
                full_name: "Org One".to_string(),
                ..OrgRow::default()
            }],
            users: vec![UserRow {
                id: "user-1".to_string(),
                authz_id: "authz-user-1".to_string(),
                username: "alice".to_string(),
                ..UserRow::default()
            }],
            global_groups: vec![GlobalGroupRow {
                id: "global-admins".to_string(),
                authz_id: "authz-global-admins".to_string(),
                name: "server-admins".to_string(),
                ..GlobalGroupRow::default()
            }],
            authz_entities: vec![
                AuthzEntityRow {
                    authz_id: "authz-global-admins".to_string(),
                    entity_type: "group".to_string(),
                    ..AuthzEntityRow::default()
                },
                AuthzEntityRow {
                    authz_id: "authz-user-1".to_string(),
                    entity_type: "actor".to_string(),
                    ..AuthzEntityRow::default()
                },
            ],
            authz_group_actors: vec![AuthzGroupActorRow {
                group_authz_id: "authz-global-admins".to_string(),
                actor_authz_id: "authz-user-1".to_string(),
            }],
            authz_acl_actor: vec![AuthzAclActorRow {
                target_authz_id: "authz-global-admins".to_string(),
                permission: "read".to_string(),
                actor_authz_id: "authz-user-1".to_string(),
            }],
            ..StoreSnapshot::default()
        },
    }
}

fn write(dir: &Path, snapshot: &Snapshot) -> PathBuf {
    let path = dir.join("snapshot.json");
    write_snapshot(snapshot, &path).unwrap();
    path
}

#[test]
fn rejects_non_empty_target_and_accepts_empty_one() {
    let target = tempfile::tempdir().unwrap();
    let input = write(target.path(), &snapshot(Some("goiardi")));

    import(target.path(), &input).unwrap();

    let error = import(target.path(), &input).unwrap_err();
    assert!(matches!(error, MigrateError::TargetNotEmpty { .. }));
    assert!(error.to_string().contains("do not merge"));
}

#[test]
fn accepts_a_target_with_only_the_bootstrap_superuser() {
    let target = tempfile::tempdir().unwrap();
    let input = write(target.path(), &snapshot(Some("goiardi")));
    let database = target.path().join("microwave.db");
    let bootstrap = microwave_store::SqliteStore::open(&database).unwrap();
    bootstrap
        .create_user(
            "pivotal",
            "pivotal@example.com",
            "bcrypt",
            "pivotal-hash",
            "pivotal-salt",
            true,
        )
        .unwrap();
    let bootstrap_admins = bootstrap
        .find_global_group("server-admins")
        .unwrap()
        .expect("create_user bootstraps server-admins");
    drop(bootstrap);

    import(target.path(), &input).unwrap();

    let store = microwave_store::SqliteStore::open(&database).unwrap();
    // (b) the snapshot's own row was dropped, not added next to the bootstrap one.
    let groups = store.list_global_groups().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].authz_id, bootstrap_admins.authz_id);

    // (c) membership now points at the bootstrap group's authz id.
    let members = store
        .list_group_members("org-1", &bootstrap_admins.authz_id)
        .unwrap();
    assert!(members.users.contains(&"alice".to_string()));

    let connection = rusqlite::Connection::open(&database).unwrap();
    // (d) the bootstrap group keeps its own entity row.
    let owned_entities: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM authz_entities WHERE authz_id = ?1 AND entity_type = 'group'",
            [&bootstrap_admins.authz_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(owned_entities, 1);
    // The rewritten snapshot entity is gone instead of dangling.
    let snapshot_entities: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM authz_entities WHERE authz_id = 'authz-global-admins'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(snapshot_entities, 0);
    // The ACL actor target was rewritten too.
    let rewritten_acl: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM authz_acl_actor WHERE target_authz_id = ?1 AND actor_authz_id = 'authz-user-1'",
            [&bootstrap_admins.authz_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rewritten_acl, 1);
}

#[test]
fn chef_snapshot_still_merges() {
    let target = tempfile::tempdir().unwrap();
    let input = write(target.path(), &snapshot(None));

    import(target.path(), &input).unwrap();
    import(target.path(), &input).unwrap();
}
