//! Goiardi snapshots land in a fresh target because their import does not
//! merge; Chef API snapshots keep their idempotent upsert behaviour.

use microwave_migrate::{
    import, write_snapshot, MigrateError, Snapshot, SnapshotSource, SnapshotSourceKind,
    SNAPSHOT_FORMAT, SNAPSHOT_VERSION,
};
use microwave_store::{OrgRow, StoreSnapshot, UserRow};
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
    let bootstrap =
        microwave_store::SqliteStore::open(&target.path().join("microwave.db")).unwrap();
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
    drop(bootstrap);

    import(target.path(), &input).unwrap();
}

#[test]
fn chef_snapshot_still_merges() {
    let target = tempfile::tempdir().unwrap();
    let input = write(target.path(), &snapshot(None));

    import(target.path(), &input).unwrap();
    import(target.path(), &input).unwrap();
}
