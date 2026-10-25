use flate2::read::GzDecoder;
use microwave_ctl::{Cli, Command};
use microwave_store::{NewNode, Org, SqliteStore};
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

const NODE_BODY: &str = r#"{"name":"web1","chef_type":"node","chef_environment":"production","default_attributes":{"tier":"prod","secret":"node-attribute-secret"},"normal":{"tags":["blue"]},"run_list":["role[web]"]}"#;
const SECOND_NODE_BODY: &str =
    r#"{"name":"beta1","chef_environment":"_default","automatic_attributes":{"platform":"linux"}}"#;
const BROKEN_NODE_BODY: &str = "this is not json";
const ENVIRONMENT_BODY: &str = r#"{"name":"production","chef_type":"environment","default_attributes":{"tier":"prod"},"override_attributes":{},"cookbook_versions":{}}"#;
const ROLE_BODY: &str = r#"{"name":"web","chef_type":"role","default_attributes":{"port":8080},"override_attributes":{},"run_list":["recipe[app::default]"]}"#;
const DATA_BAG_ITEM_BODY: &str =
    r#"{"id":"database","host":"db.example.com","password":"data-bag-secret"}"#;

const DB_SECRET: &str = "database-secret-value-0f8f0ac";
const CONFIG_SECRET: &str = "config-secret-value-4b1c9de";
const BLOB_SECRET: &str = "bookshelf-blob-secret-77c28fa";
const ATTRIBUTE_SECRET: &str = "node-attribute-secret";

fn toml_path(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn write_config(root: &Path, data_dir: &Path, extra: &str) -> PathBuf {
    let database = data_dir.join("microwave.db");
    let path = root.join("microwave.toml");
    let text = format!(
        "[paths]\ndata_dir = \"{}\"\n\n[database]\ndriver = \"sqlite\"\nurl = \"sqlite://{}\"\n{}",
        toml_path(data_dir),
        toml_path(&database),
        extra
    );
    std::fs::write(&path, text).unwrap();
    path
}

fn create_org(store: &SqliteStore, name: &str) -> Org {
    let org = store.create_org(name, name).unwrap();
    for container in ["nodes", "environments", "roles", "data"] {
        store.create_container_for_org(&org.id, container).unwrap();
    }
    org
}

fn run(config: &Path, command: Command) -> Result<(), microwave_ctl::CtlError> {
    microwave_ctl::dispatch(Cli {
        config: Some(config.to_path_buf()),
        command,
    })
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn archive_entries(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let file = std::fs::File::open(path).unwrap();
    let decoder = GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    let mut entries = BTreeMap::new();
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        entries.insert(name, data);
    }
    entries
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

#[test]
fn filtered_dump_redacts_attributes_and_filters_by_org() {
    let root = tempfile::tempdir().unwrap();
    let data_dir = root.path().join("data");
    let config = write_config(root.path(), &data_dir, "");

    let store = SqliteStore::open(&data_dir.join("microwave.db")).unwrap();
    let acme = create_org(&store, "acme");
    let beta = create_org(&store, "beta");

    let node = store
        .create_node(
            &acme.id,
            NewNode {
                name: "web1",
                body: NODE_BODY,
                environment: "production",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &acme.authz_id,
            },
        )
        .unwrap();
    let broken_node = store
        .create_node(
            &acme.id,
            NewNode {
                name: "broken",
                body: BROKEN_NODE_BODY,
                environment: "production",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &acme.authz_id,
            },
        )
        .unwrap();
    store
        .create_node(
            &beta.id,
            NewNode {
                name: "beta1",
                body: SECOND_NODE_BODY,
                environment: "_default",
                policy_name: None,
                policy_group: None,
                creator_authz_id: &beta.authz_id,
            },
        )
        .unwrap();
    let environment = store
        .create_environment(&acme.id, "production", ENVIRONMENT_BODY, &acme.authz_id)
        .unwrap();
    let role = store
        .create_role(&acme.id, "web", ROLE_BODY, &acme.authz_id)
        .unwrap();
    let client = store
        .create_client(&acme.id, "web-client", false, true)
        .unwrap();
    store
        .create_data_bag(&acme.id, "secrets", &acme.authz_id)
        .unwrap();
    let item = store
        .create_data_bag_item(&acme.id, "secrets", "database", DATA_BAG_ITEM_BODY)
        .unwrap();
    drop(store);

    let dump = root.path().join("dump-all");
    run(
        &config,
        Command::FilteredDump {
            output: dump.clone(),
            org: None,
        },
    )
    .unwrap();

    for name in [
        "organizations.json",
        "nodes.json",
        "environments.json",
        "roles.json",
        "data_bag_items.json",
        "clients.json",
    ] {
        assert!(dump.join(name).is_file(), "missing {name}");
    }

    let organizations = read_json(&dump.join("organizations.json"));
    let organizations = organizations.as_array().unwrap();
    assert_eq!(organizations.len(), 2);
    let acme_org = organizations
        .iter()
        .find(|org| org["name"] == "acme")
        .unwrap();
    assert_eq!(acme_org["id"], acme.id);
    assert!(acme_org["created_at"].as_str().is_some());
    assert!(acme_org["updated_at"].as_str().is_some());
    assert!(acme_org.get("authz_id").is_none());

    let nodes = read_json(&dump.join("nodes.json"));
    let nodes = nodes.as_array().unwrap();
    assert_eq!(nodes.len(), 3);
    let dumped_node = nodes.iter().find(|node| node["name"] == "web1").unwrap();
    assert_eq!(dumped_node["org_id"], acme.id);
    assert_eq!(dumped_node["id"], node.id);
    assert_eq!(dumped_node["environment"], "production");
    assert_eq!(dumped_node["created_at"], node.created_at);
    assert_eq!(dumped_node["updated_at"], node.updated_at);
    assert_eq!(
        dumped_node["serialized_object"]["default_attributes"],
        "removed 2 entries"
    );
    assert_eq!(
        dumped_node["serialized_object"]["normal"],
        "removed 1 entries"
    );
    assert_eq!(dumped_node["serialized_object"]["run_list"][0], "role[web]");
    let broken = nodes.iter().find(|node| node["name"] == "broken").unwrap();
    assert_eq!(broken["id"], broken_node.id);
    let broken_object = &broken["serialized_object"];
    assert_eq!(
        broken_object["error"],
        "could not parse node serialized_object as json"
    );

    let environments = read_json(&dump.join("environments.json"));
    let environments = environments.as_array().unwrap();
    assert_eq!(environments.len(), 1);
    assert_eq!(environments[0]["org_id"], acme.id);
    assert_eq!(environments[0]["id"], environment.id);
    assert_eq!(environments[0]["name"], "production");
    assert_eq!(
        environments[0]["serialized_object"]["default_attributes"],
        "removed 1 entries"
    );
    assert_eq!(
        environments[0]["serialized_object"]["override_attributes"],
        "removed 0 entries"
    );

    let roles = read_json(&dump.join("roles.json"));
    let roles = roles.as_array().unwrap();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0]["org_id"], acme.id);
    assert_eq!(roles[0]["id"], role.id);
    assert_eq!(roles[0]["name"], "web");
    assert_eq!(
        roles[0]["serialized_object"]["default_attributes"],
        "removed 1 entries"
    );

    let clients = read_json(&dump.join("clients.json"));
    let clients = clients.as_array().unwrap();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0]["org_id"], acme.id);
    assert_eq!(clients[0]["id"], client.id);
    assert_eq!(clients[0]["name"], "web-client");
    assert_eq!(clients[0]["validator"], false);
    assert_eq!(clients[0]["admin"], true);
    assert_eq!(clients[0]["created_at"], client.created_at);
    assert_eq!(clients[0]["updated_at"], client.updated_at);

    let items = read_json(&dump.join("data_bag_items.json"));
    let items = items.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["org_id"], acme.id);
    assert_eq!(items[0]["id"], item.id);
    assert_eq!(items[0]["data_bag_name"], "secrets");
    assert_eq!(items[0]["item_name"], "database");
    assert_eq!(items[0]["created_at"], item.created_at);
    assert_eq!(items[0]["updated_at"], item.updated_at);
    assert_eq!(items[0]["serialized_object"]["id"], "database");
    assert_eq!(items[0]["serialized_object"]["password"], "value removed");
    assert_eq!(items[0]["serialized_object"]["host"], "value removed");

    let nodes_text = std::fs::read_to_string(dump.join("nodes.json")).unwrap();
    assert!(!nodes_text.contains(ATTRIBUTE_SECRET));
    let items_text = std::fs::read_to_string(dump.join("data_bag_items.json")).unwrap();
    assert!(!items_text.contains("data-bag-secret"));

    let filtered = root.path().join("dump-acme");
    run(
        &config,
        Command::FilteredDump {
            output: filtered.clone(),
            org: Some("acme".into()),
        },
    )
    .unwrap();

    let organizations = read_json(&filtered.join("organizations.json"));
    let organizations = organizations.as_array().unwrap();
    assert_eq!(organizations.len(), 1);
    assert_eq!(organizations[0]["name"], "acme");
    let nodes = read_json(&filtered.join("nodes.json"));
    let nodes = nodes.as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    assert!(nodes.iter().all(|node| node["org_id"] == acme.id));
    assert!(nodes.iter().all(|node| node["name"] != "beta1"));

    let error = run(
        &config,
        Command::FilteredDump {
            output: root.path().join("dump-missing"),
            org: Some("missing".into()),
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("does not exist"),
        "unexpected error: {error}"
    );
}

#[test]
fn gather_logs_writes_a_redacted_archive_and_refuses_an_existing_path() {
    let root = tempfile::tempdir().unwrap();
    let data_dir = root.path().join("data");
    let config = write_config(
        root.path(),
        &data_dir,
        &format!(
            "\n[search]\nbackend = \"opensearch\"\nurl = \"http://user:hunter2@search.example:9200/\"\n\
             \n[object_store]\nbackend = \"s3\"\nendpoint = \"https://objects.example\"\n\
             bucket = \"microwave\"\nsecret_key = \"{CONFIG_SECRET}\"\naccess_key = \"access-key-value\"\n"
        ),
    );

    let store = SqliteStore::open(&data_dir.join("microwave.db")).unwrap();
    store
        .set_secret("bookshelf", "secret_access_key", DB_SECRET)
        .unwrap();
    drop(store);

    // Contents must never be collected; only names and sizes are reported.
    std::fs::create_dir_all(data_dir.join("bookshelf")).unwrap();
    std::fs::write(data_dir.join("bookshelf").join("blob"), BLOB_SECRET).unwrap();
    std::fs::write(data_dir.join("private.pem"), "PRIVATE KEY MATERIAL").unwrap();

    let archive_path = root.path().join("logs.tar.gz");
    run(
        &config,
        Command::GatherLogs {
            output: Some(archive_path.clone()),
        },
    )
    .unwrap();

    assert!(archive_path.is_file());
    let entries = archive_entries(&archive_path);
    let names: Vec<&str> = entries.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        vec![
            "README.txt",
            "config.toml",
            "data-listing.txt",
            "journal.txt",
            "status.txt",
            "system.txt"
        ]
    );

    let readme = String::from_utf8(entries["README.txt"].clone()).unwrap();
    assert!(readme.contains("bookshelf"));
    assert!(readme.contains("database"));

    let system = String::from_utf8(entries["system.txt"].clone()).unwrap();
    assert!(system.contains("Data directory:"));
    assert!(system.contains("Database driver: sqlite"));
    assert!(system.contains("SQLite database"));

    let status = String::from_utf8(entries["status.txt"].clone()).unwrap();
    assert!(status.contains("database_readable: true"));
    assert!(status.contains("superuser: pivotal (absent)"));
    assert!(status.contains("maintenance: disabled"));

    let config_entry = String::from_utf8(entries["config.toml"].clone()).unwrap();
    assert!(config_entry.contains("[server]"));
    assert!(config_entry.contains("tls_enabled = false"));
    assert!(config_entry.contains("[paths]"));
    assert!(config_entry.contains("data_dir ="));
    assert!(config_entry.contains("driver = \"sqlite\""));
    assert!(config_entry.contains("backend = \"opensearch\""));
    assert!(config_entry.contains("http://search.example:9200/"));
    assert!(config_entry.contains("bucket = \"microwave\""));

    let listing = String::from_utf8(entries["data-listing.txt"].clone()).unwrap();
    assert!(listing.contains("microwave.db"));
    assert!(listing.contains("bookshelf"));

    for (name, contents) in &entries {
        assert!(
            !contains(contents, DB_SECRET),
            "{name} leaked a database secret"
        );
        assert!(
            !contains(contents, CONFIG_SECRET),
            "{name} leaked a configuration secret"
        );
        assert!(
            !contains(contents, BLOB_SECRET),
            "{name} leaked a bookshelf blob"
        );
        assert!(
            !contains(contents, "hunter2"),
            "{name} leaked URL credentials"
        );
        assert!(
            !contains(contents, "PRIVATE KEY MATERIAL"),
            "{name} leaked a private key"
        );
        assert!(
            !contains(contents, "access-key-value"),
            "{name} leaked an access key"
        );
    }

    let existing = root.path().join("already-there.tar.gz");
    std::fs::write(&existing, "keep me").unwrap();
    let error = run(
        &config,
        Command::GatherLogs {
            output: Some(existing.clone()),
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("already exists"),
        "unexpected error: {error}"
    );
    assert_eq!(std::fs::read_to_string(existing).unwrap(), "keep me");
}
