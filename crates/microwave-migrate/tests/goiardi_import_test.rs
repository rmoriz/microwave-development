mod common;

use common::{full_goiardi_routes, options, start_server};
use microwave_migrate::{from_goiardi_with_options, import, read_snapshot, MigrateError};
use microwave_store::{ActorType, Permission, SqliteStore};

#[test]
fn import_roundtrip_keeps_keys_acls_and_blobs_and_refuses_a_second_import() {
    let (routes, checksum, body) = full_goiardi_routes();
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
    assert!(store
        .find_data_bag_item(&org.id, "secrets", "db")
        .unwrap()
        .is_some());
    let cookbook = store
        .find_cookbook_version(&org.id, "app", "1.2.3")
        .unwrap()
        .unwrap();
    assert_eq!(cookbook.checksums, vec![checksum.clone()]);
    let blob = bookshelf.join(format!("organization-{}/checksum-{checksum}", org.id));
    assert_eq!(std::fs::read(blob).unwrap(), body);

    let error = import(data.path(), &snapshot).unwrap_err();
    assert!(matches!(error, MigrateError::TargetNotEmpty { .. }));
    let records = records.lock().unwrap();
    assert!(!records.is_empty());
    assert!(records.iter().all(|record| record.method == "GET"));
    assert!(records.iter().all(|record| record.verified));
}

#[test]
fn source_without_an_admin_user_still_imports_a_synthesized_admin() {
    let (routes, _, _) = full_goiardi_routes();
    let routes = routes
        .into_iter()
        .filter(|(path, _)| !path.starts_with("/users"))
        .collect();
    let (url, _) = start_server(routes);
    let data = tempfile::tempdir().unwrap();
    let snapshot_path = data.path().join("snapshot.json");
    let mut options = options(&url, data.path());
    options.bookshelf_dir = Some(data.path().join("bookshelf"));

    let summary = from_goiardi_with_options(&options).unwrap();
    assert!(summary
        .warnings
        .iter()
        .any(|warning| warning.contains("synthesized")));

    let snapshot = read_snapshot(&snapshot_path).unwrap();
    let org_id = snapshot.data.orgs[0].id.clone();
    import(data.path(), &snapshot_path).unwrap();

    let store = SqliteStore::open(&data.path().join("microwave.db")).unwrap();
    let admin = store
        .find_user("admin")
        .unwrap()
        .expect("synthesized admin");
    assert!(admin.admin);
    let orgs = store.list_user_orgs(&admin.id).unwrap();
    assert!(orgs.iter().any(|org| org.id == org_id));
}
