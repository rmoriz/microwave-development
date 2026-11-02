mod common;

use common::{full_goiardi_routes, options, start_server};
use microwave_migrate::{from_goiardi_with_options, import, MigrateError};
use microwave_store::{ActorType, Permission, SqliteStore};

#[test]
fn import_roundtrip_keeps_keys_acls_and_blobs_and_refuses_a_second_import() {
    let (routes, checksum) = full_goiardi_routes();
    let (url, records) = start_server(routes);
    let data = tempfile::tempdir().unwrap();
    let snapshot = data.path().join("snapshot.json");
    let bookshelf = data.path().join("bookshelf");
    let mut options = options(&url, data.path());
    options.bookshelf_dir = Some(bookshelf.clone());
    from_goiardi_with_options(&options).unwrap();
    import(data.path(), &snapshot).unwrap();

    let store = SqliteStore::open(&data.path().join("microwave.db")).unwrap();
    let org = store.find_org("acme").unwrap().unwrap();
    let admin = store.find_user("admin").unwrap().unwrap();
    let client = store.find_client(&org.id, "web1").unwrap().unwrap();
    assert!(store
        .find_actor_key(&client.id, ActorType::Client, "default")
        .unwrap()
        .is_some());
    assert!(store
        .check_permission(&admin.authz_id, &org.authz_id, Permission::Grant)
        .unwrap());
    let nodes = store.find_container(&org.id, "nodes").unwrap().unwrap();
    assert!(store
        .check_permission(&client.authz_id, &nodes.authz_id, Permission::Read)
        .unwrap());
    assert!(store.find_node(&org.id, "web1").unwrap().is_some());
    assert!(bookshelf
        .join(format!("organization-{}/checksum-{checksum}", org.id))
        .exists());

    let error = import(data.path(), &snapshot).unwrap_err();
    assert!(matches!(error, MigrateError::TargetNotEmpty { .. }));
    let records = records.lock().unwrap();
    assert!(!records.is_empty());
    assert!(records.iter().all(|record| record.method == "GET"));
    assert!(records.iter().all(|record| record.verified));
}
