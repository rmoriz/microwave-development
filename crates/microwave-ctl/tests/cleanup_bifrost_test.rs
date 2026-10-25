//! Integration coverage for `microwave-ctl cleanup-bifrost`.
//!
//! `cleanup-bifrost` removes `authz_entities` rows that no identity or object
//! table references. The first test exercises the backend-neutral repository
//! methods on an in-memory SQLite store; the second drives the CLI against an
//! on-disk database.

use microwave_ctl::{Cli, Command, CtlError};
use microwave_store::{NewNode, Permission, SqliteStore};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Seeded {
    user_authz_id: String,
    client_authz_id: String,
    group_authz_id: String,
    node_authz_id: String,
    orphan_authz_id: String,
}

/// Create one live actor of each kind, a stored object, and one authz entity
/// that nothing references.
fn seed(store: &SqliteStore) -> Seeded {
    let org = store.create_org("acme", "Acme Corp").unwrap();
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
    let client = store.create_client(&org.id, "robot", false, false).unwrap();
    let group = store.create_group_for_org(&org.id, "admins").unwrap();
    // Nodes hang off the organization's `nodes` container, which must exist
    // before the object can be created.
    store.create_container_for_org(&org.id, "nodes").unwrap();
    let node = store
        .create_node(
            &org.id,
            NewNode {
                name: "node1",
                body: "{}",
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &user.authz_id,
            },
        )
        .unwrap();
    // `create_actor` inserts only an `authz_entities` row, so this entity has
    // no corresponding users/clients row and is the orphan cleanup must find.
    let orphan_authz_id = store.create_actor().unwrap();

    Seeded {
        user_authz_id: user.authz_id,
        client_authz_id: client.authz_id,
        group_authz_id: group.authz_id,
        node_authz_id: node.authz_id,
        orphan_authz_id,
    }
}

fn referenced_ids(seeded: &Seeded) -> [&str; 4] {
    [
        &seeded.user_authz_id,
        &seeded.client_authz_id,
        &seeded.group_authz_id,
        &seeded.node_authz_id,
    ]
}

fn assert_referenced_entities_exist(store: &SqliteStore, seeded: &Seeded) {
    for authz_id in referenced_ids(seeded) {
        store.get_acl_entries(authz_id).unwrap_or_else(|error| {
            panic!("referenced authz entity {authz_id} must remain: {error}")
        });
    }
}

#[test]
fn repository_counts_and_deletes_only_orphans() {
    let store = SqliteStore::open_in_memory().unwrap();
    let seeded = seed(&store);

    assert_eq!(store.count_orphan_authz_entities().unwrap(), 1);
    assert_referenced_entities_exist(&store, &seeded);

    // A grant that targets an orphan does not keep the orphan alive. This
    // also proves the delete path cleans up ACL rows before removing the
    // entity, even though the entity itself is no longer referenced.
    store
        .set_acl(
            &seeded.orphan_authz_id,
            Permission::Read,
            Some(&seeded.user_authz_id),
            None,
        )
        .unwrap();

    assert_eq!(store.count_orphan_authz_entities().unwrap(), 1);
    assert_eq!(store.delete_orphan_authz_entities().unwrap(), 1);
    assert_eq!(store.count_orphan_authz_entities().unwrap(), 0);
    assert!(store.get_acl_entries(&seeded.orphan_authz_id).is_err());
    assert_referenced_entities_exist(&store, &seeded);

    // A second cleanup run finds nothing and leaves live entities alone.
    assert_eq!(store.delete_orphan_authz_entities().unwrap(), 0);
    assert_eq!(store.count_orphan_authz_entities().unwrap(), 0);
    assert_referenced_entities_exist(&store, &seeded);
}

fn write_config(dir: &Path, data_dir: &Path, db_path: &Path) -> PathBuf {
    let path = dir.join("microwave.toml");
    std::fs::write(
        &path,
        format!(
            "[paths]\n\
             data_dir = '{}'\n\
             \n\
             [database]\n\
             driver = \"sqlite\"\n\
             url = \"sqlite://{}\"\n",
            data_dir.display(),
            db_path.display()
        ),
    )
    .unwrap();
    path
}

fn run_cleanup(
    config: &Path,
    estimate_only: bool,
    wait_time: u64,
    force_cleanup: bool,
) -> Result<(), CtlError> {
    microwave_ctl::dispatch(Cli {
        config: Some(config.to_path_buf()),
        command: Command::CleanupBifrost {
            estimate_only,
            wait_time,
            force_cleanup,
        },
    })
}

#[test]
fn cli_estimate_only_and_delete_against_sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let db_path = data_dir.join("microwave.db");
    let config = write_config(dir.path(), &data_dir, &db_path);

    let seeded = {
        let store = SqliteStore::open(&db_path).unwrap();
        let seeded = seed(&store);
        assert_eq!(store.count_orphan_authz_entities().unwrap(), 1);
        seeded
    };

    // --estimate-only reports the count and deletes nothing.
    run_cleanup(&config, true, 0, false).unwrap();
    let store = SqliteStore::open(&db_path).unwrap();
    assert_eq!(store.count_orphan_authz_entities().unwrap(), 1);
    assert!(store.get_acl_entries(&seeded.orphan_authz_id).is_ok());
    assert_referenced_entities_exist(&store, &seeded);
    drop(store);

    // The default removes the orphan and leaves every referenced entity.
    run_cleanup(&config, false, 0, false).unwrap();
    let store = SqliteStore::open(&db_path).unwrap();
    assert_eq!(store.count_orphan_authz_entities().unwrap(), 0);
    assert!(store.get_acl_entries(&seeded.orphan_authz_id).is_err());
    assert_referenced_entities_exist(&store, &seeded);
    drop(store);

    // A second run is idempotent.
    run_cleanup(&config, false, 0, false).unwrap();

    // --force-cleanup is accepted even though Microwave has no legacy
    // cleanup tracking tables.
    run_cleanup(&config, false, 0, true).unwrap();

    // --wait-time delays the count before touching the database.
    let started = Instant::now();
    run_cleanup(&config, true, 1, false).unwrap();
    assert!(started.elapsed() >= Duration::from_secs(1));
}
