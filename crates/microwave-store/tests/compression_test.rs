use microwave_store::{
    encode_serialized, Compression, NewNode, SqliteStore, StoreError, COMPRESSED_PREFIX,
};
use rusqlite::Connection;
use tempfile::TempDir;

const NODE_BODY: &str =
    r#"{"name":"node-1","chef_environment":"_default","run_list":["recipe[apt]"]}"#;
const ROLE_BODY: &str = r#"{"name":"web","run_list":["recipe[nginx]"]}"#;
const ENV_BODY: &str = r#"{"name":"prod","default_attributes":{"a":1}}"#;
const ITEM_BODY: &str = r#"{"id":"db","password":"secret"}"#;
const POLICY_BODY: &str = r#"{"name":"base","revision_id":"rev-1","run_list":["recipe[apt]"]}"#;
const GROUP_BODY: &str = r#"{"name":"prod-group","policies":{}}"#;
const COOKBOOK_BODY: &str = r#"{"name":"apt","version":"1.0.0"}"#;
const ARTIFACT_BODY: &str = r#"{"name":"apt-artifact","version":"1.0.0"}"#;

fn database(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("microwave.db")
}

fn fixture(store: &SqliteStore) -> (String, String) {
    let org = store
        .create_org("compression-org", "Compression Org")
        .unwrap();
    let creator = store.create_actor().unwrap();
    for container in [
        "clients",
        "containers",
        "cookbooks",
        "cookbook_artifacts",
        "data",
        "environments",
        "groups",
        "nodes",
        "policies",
        "policy_groups",
        "roles",
        "sandboxes",
    ] {
        store
            .create_container_for_org_with_acl(&org.id, container, &creator)
            .unwrap();
    }
    (org.id, creator)
}

fn seed(store: &SqliteStore) -> (String, String) {
    let (org_id, creator) = fixture(store);
    store
        .create_node(
            &org_id,
            NewNode {
                name: "node-1",
                body: NODE_BODY,
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &creator,
            },
        )
        .unwrap();
    store
        .create_role(&org_id, "web", ROLE_BODY, &creator)
        .unwrap();
    store
        .create_environment(&org_id, "prod", ENV_BODY, &creator)
        .unwrap();
    store.create_data_bag(&org_id, "secrets", &creator).unwrap();
    store
        .create_data_bag_item(&org_id, "secrets", "db", ITEM_BODY)
        .unwrap();
    store
        .create_cookbook_version(&org_id, "apt", "1.0.0", COOKBOOK_BODY, &[], false, &creator)
        .unwrap();
    store
        .create_cookbook_artifact_version(
            &org_id,
            "apt-artifact",
            "id-1",
            ARTIFACT_BODY,
            &[],
            &creator,
        )
        .unwrap();
    store
        .create_policy_revision(&org_id, "base", "rev-1", POLICY_BODY, &creator)
        .unwrap();
    store
        .create_policy_group(&org_id, "prod-group", GROUP_BODY, &creator)
        .unwrap();
    (org_id, creator)
}

fn raw_serialized(path: &std::path::Path, table: &str, name_column: &str, name: &str) -> String {
    let connection = Connection::open(path).unwrap();
    connection
        .query_row(
            &format!("SELECT serialized_object FROM {table} WHERE {name_column} = ?1"),
            [name],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn sqlite_writes_compressed_values_and_reads_plain_json() {
    let dir = TempDir::new().unwrap();
    let path = database(&dir);
    let store = SqliteStore::open(&path).unwrap();
    let (org_id, _) = seed(&store);

    // Every relevant serialized_object column is stored with the magic prefix.
    for (table, name_column, name) in [
        ("nodes", "name", "node-1"),
        ("roles", "name", "web"),
        ("environments", "name", "prod"),
        ("data_bag_items", "item_name", "db"),
        ("cookbook_versions", "name", "apt"),
        ("cookbook_artifact_versions", "identifier", "id-1"),
        ("policy_revisions", "name", "base"),
        ("policy_groups", "name", "prod-group"),
    ] {
        let raw = raw_serialized(&path, table, name_column, name);
        assert!(
            raw.starts_with(COMPRESSED_PREFIX),
            "{table} was not compressed"
        );
        assert!(!raw.contains("run_list"), "{table} leaked plain JSON");
    }

    assert_eq!(
        store
            .find_node(&org_id, "node-1")
            .unwrap()
            .unwrap()
            .serialized_object,
        NODE_BODY
    );
    assert_eq!(
        store
            .find_role(&org_id, "web")
            .unwrap()
            .unwrap()
            .serialized_object,
        ROLE_BODY
    );
    assert_eq!(
        store
            .find_environment(&org_id, "prod")
            .unwrap()
            .unwrap()
            .serialized_object,
        ENV_BODY
    );
    assert_eq!(
        store
            .find_data_bag_item(&org_id, "secrets", "db")
            .unwrap()
            .unwrap()
            .serialized_object,
        ITEM_BODY
    );
    assert_eq!(
        store
            .find_cookbook_version(&org_id, "apt", "1.0.0")
            .unwrap()
            .unwrap()
            .manifest,
        COOKBOOK_BODY
    );
    assert_eq!(
        store
            .find_cookbook_artifact_version(&org_id, "apt-artifact", "id-1")
            .unwrap()
            .unwrap()
            .manifest,
        ARTIFACT_BODY
    );
    assert_eq!(
        store
            .find_policy_revision(&org_id, "base", "rev-1")
            .unwrap()
            .unwrap()
            .serialized_object,
        POLICY_BODY
    );
    assert_eq!(
        store
            .find_policy_group(&org_id, "prod-group")
            .unwrap()
            .unwrap()
            .serialized_object,
        GROUP_BODY
    );
}

#[test]
fn legacy_plain_json_remains_readable() {
    let dir = TempDir::new().unwrap();
    let path = database(&dir);
    let store = SqliteStore::open_with_compression(&path, Compression::None).unwrap();
    let (org_id, _) = seed(&store);
    drop(store);

    // The writer used no compression, so the row is legacy plain JSON.
    assert_eq!(raw_serialized(&path, "nodes", "name", "node-1"), NODE_BODY);

    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(
        store
            .find_node(&org_id, "node-1")
            .unwrap()
            .unwrap()
            .serialized_object,
        NODE_BODY
    );
}

#[test]
fn corrupt_compressed_values_return_a_store_error() {
    let dir = TempDir::new().unwrap();
    let path = database(&dir);
    let store = SqliteStore::open(&path).unwrap();
    let (org_id, _) = seed(&store);
    drop(store);

    for corrupt in ["mwz1:not-base64!", "mwz1:bG90IHpsaWI="] {
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE nodes SET serialized_object = ?1 WHERE name = 'node-1'",
                [corrupt],
            )
            .unwrap();
        drop(connection);

        let store = SqliteStore::open(&path).unwrap();
        let error = store.find_node(&org_id, "node-1").unwrap_err();
        assert!(
            matches!(error, StoreError::Compression(_)),
            "expected compression error, got {error:?}"
        );
    }
}

#[test]
fn snapshots_are_decoded_and_round_trip_through_a_second_store() {
    let source_dir = TempDir::new().unwrap();
    let source = SqliteStore::open(&database(&source_dir)).unwrap();
    let (org_id, _) = seed(&source);
    let exported = source.export_snapshot().unwrap();

    assert_eq!(
        exported.nodes[0].serialized_object.as_deref(),
        Some(NODE_BODY)
    );
    assert_eq!(
        exported.policy_revisions[0].serialized_object.as_deref(),
        Some(POLICY_BODY)
    );
    assert_eq!(
        exported.cookbook_artifact_versions[0]
            .serialized_object
            .as_deref(),
        Some(ARTIFACT_BODY)
    );
    let json = serde_json::to_string(&exported).unwrap();
    assert!(!json.contains(COMPRESSED_PREFIX));

    let target_dir = TempDir::new().unwrap();
    let target_path = database(&target_dir);
    let target = SqliteStore::open(&target_path).unwrap();
    target.import_snapshot(&exported).unwrap();

    assert_eq!(
        target
            .find_node(&org_id, "node-1")
            .unwrap()
            .unwrap()
            .serialized_object,
        NODE_BODY
    );
    assert!(raw_serialized(&target_path, "nodes", "name", "node-1").starts_with(COMPRESSED_PREFIX));
    assert_eq!(exported, target.export_snapshot().unwrap());
}

#[test]
fn legacy_plain_snapshots_import_into_a_compressed_store() {
    let source_dir = TempDir::new().unwrap();
    let source = SqliteStore::open(&database(&source_dir)).unwrap();
    let (_org_id, _) = seed(&source);
    let mut snapshot = source.export_snapshot().unwrap();
    snapshot.nodes[0].serialized_object = Some(r#"{"name":"legacy"}"#.to_string());

    let target_dir = TempDir::new().unwrap();
    let target_path = database(&target_dir);
    let target = SqliteStore::open(&target_path).unwrap();
    target.import_snapshot(&snapshot).unwrap();

    let node = target
        .find_node(&snapshot.nodes[0].org_id, &snapshot.nodes[0].name)
        .unwrap()
        .unwrap();
    assert_eq!(node.serialized_object, r#"{"name":"legacy"}"#);
    assert!(
        raw_serialized(&target_path, "nodes", "name", &snapshot.nodes[0].name)
            .starts_with(COMPRESSED_PREFIX)
    );
}

#[test]
fn encode_none_is_plain_and_decodes_like_legacy() {
    let encoded = encode_serialized(NODE_BODY, Compression::None).unwrap();
    assert_eq!(encoded, NODE_BODY);
    assert_eq!(
        microwave_store::decode_serialized(&encoded).unwrap(),
        NODE_BODY
    );
}
