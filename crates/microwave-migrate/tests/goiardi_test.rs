mod common;

use common::{blob_fixture, bytes_route, json_route, options, raw_route, start_server, Route};
use microwave_migrate::{crawl_from_goiardi, from_goiardi_with_options, GoiardiOptions};
use serde_json::json;

const PUBLIC_KEY: &str = include_str!("../../microwave-crypto/tests/fixtures/test_key_pub.pem");

const USER: &str = r#"{"username":"admin","name":"admin","email":"ops@example.com","admin":true,"public_key":"<PUBLIC_KEY>"}"#;
const CLIENT: &str =
    r#"{"name":"web1","admin":false,"validator":false,"public_key":"<PUBLIC_KEY>"}"#;

const NODE: &str = r#"{"name":"web1","chef_environment":"_default","normal":{},"run_list":[]}"#;
const ROLE: &str =
    r#"{"name":"web","run_list":[],"default_attributes":{},"override_attributes":{}}"#;
const ENV: &str = r#"{"name":"production","cookbook_versions":{},"default_attributes":{},"override_attributes":{}}"#;
const DEFAULT_ENV: &str = r#"{"name":"_default","cookbook_versions":{},"default_attributes":{},"override_attributes":{}}"#;
const BAG_ITEM: &str = r#"{"id":"db","password":"x"}"#;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn crawl_objects_routes() -> Vec<(&'static str, Route)> {
    vec![
        ("/nodes", json_route(json!({"web1": "/nodes/web1"}))),
        ("/nodes/web1", raw_route(NODE)),
        ("/roles", json_route(json!({"web": "/roles/web"}))),
        ("/roles/web", raw_route(ROLE)),
        (
            "/environments",
            json_route(json!({
                "_default": "/environments/_default",
                "production": "/environments/production",
            })),
        ),
        ("/environments/production", raw_route(ENV)),
        ("/environments/_default", raw_route(DEFAULT_ENV)),
        ("/data", json_route(json!({"secrets": "/data/secrets"}))),
        (
            "/data/secrets",
            json_route(json!({"db": "/data/secrets/db"})),
        ),
        ("/data/secrets/db", raw_route(BAG_ITEM)),
    ]
}

#[test]
fn crawls_objects_raw_and_skips_default_environment() {
    let dir = tempdir();
    let (url, _) = start_server(crawl_objects_routes());
    let (snapshot, summary) = crawl_from_goiardi(&options(&url, dir.path())).unwrap();
    assert_eq!(snapshot.data.nodes[0].name, "web1");
    assert_eq!(
        snapshot.data.nodes[0].serialized_object.as_deref(),
        Some(NODE)
    );
    assert_eq!(snapshot.data.roles[0].name, "web");
    // The source `_default` row is skipped, then the policy synthesizes one.
    assert_eq!(snapshot.data.environments.len(), 2);
    assert_eq!(snapshot.data.environments[0].name, "production");
    assert_eq!(snapshot.data.environments[1].name, "_default");
    assert_eq!(snapshot.data.data_bags[0].name, "secrets");
    assert_eq!(snapshot.data.data_bag_items[0].item_name, "db");
    assert_eq!(summary.data_bag_items, 1);
    // The node sits in `_default`, which must count as a known environment.
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
}

#[test]
fn node_in_default_environment_does_not_warn_when_the_source_omits_it() {
    let dir = tempdir();
    let mut routes = crawl_objects_routes();
    routes[4].1 = json_route(json!({}));
    let (url, _) = start_server(routes);
    let (_, summary) = crawl_from_goiardi(&options(&url, dir.path())).unwrap();
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
}

#[test]
fn warns_about_an_unknown_node_environment_and_keeps_the_node() {
    let dir = tempdir();
    let mut routes = crawl_objects_routes();
    routes[1].1 =
        raw_route(r#"{"name":"web1","chef_environment":"gone","normal":{},"run_list":[]}"#);
    let (url, _) = start_server(routes);
    let (snapshot, summary) = crawl_from_goiardi(&options(&url, dir.path())).unwrap();
    assert!(summary
        .warnings
        .iter()
        .any(|warning| warning.contains("gone")));
    assert!(snapshot.data.nodes.iter().any(|node| node.name == "web1"));
}

#[test]
fn skip_orphan_clients_keeps_validator_and_webui() {
    let dir = tempdir();
    let mut routes = crawl_objects_routes();
    routes.extend([
        (
            "/clients",
            json_route(json!({
                "web1": "/clients/web1",
                "old1": "/clients/old1",
                "chef-validator": "/clients/chef-validator",
                "chef-webui": "/clients/chef-webui",
            })),
        ),
        ("/clients/web1", raw_route(CLIENT)),
        (
            "/clients/old1",
            raw_route(r#"{"name":"old1","public_key":"<PUBLIC_KEY>"}"#),
        ),
        (
            "/clients/chef-validator",
            raw_route(r#"{"name":"chef-validator","validator":true,"public_key":"<PUBLIC_KEY>"}"#),
        ),
        (
            "/clients/chef-webui",
            raw_route(r#"{"name":"chef-webui","admin":true,"public_key":"<PUBLIC_KEY>"}"#),
        ),
    ]);
    let (url, _) = start_server(routes);

    // The unfiltered crawl shows the rows the filter has to remove.
    let unfiltered_dir = tempdir();
    let (unfiltered, _) = crawl_from_goiardi(&options(&url, unfiltered_dir.path())).unwrap();
    let old1 = unfiltered
        .data
        .clients
        .iter()
        .find(|client| client.name == "old1")
        .expect("old1 imported without the filter");
    let old1_key = unfiltered
        .data
        .actor_keys
        .iter()
        .find(|key| key.actor_id == old1.id)
        .expect("old1 has a key");
    assert!(!old1_key.public_key.is_empty());
    assert!(unfiltered
        .data
        .authz_entities
        .iter()
        .any(|entity| entity.authz_id == old1.authz_id));
    let (old1_id, old1_authz) = (old1.id.clone(), old1.authz_id.clone());

    let mut options = options(&url, dir.path());
    options.skip_orphan_clients = true;
    let (snapshot, _) = crawl_from_goiardi(&options).unwrap();
    let mut names: Vec<_> = snapshot
        .data
        .clients
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["chef-validator", "chef-webui", "web1"]);
    assert!(!snapshot
        .data
        .actor_keys
        .iter()
        .any(|key| key.actor_id == old1_id));
    assert!(!snapshot
        .data
        .authz_entities
        .iter()
        .any(|entity| entity.authz_id == old1_authz));
}

#[test]
fn crawls_users_and_clients_with_keys_and_marks_the_layout() {
    let dir = tempdir();
    let dir_path = dir.path();
    let routes = vec![
        ("/users", json_route(json!({"admin": "/users/admin"}))),
        ("/users/admin", raw_route(USER)),
        ("/clients", json_route(json!({"web1": "/clients/web1"}))),
        ("/clients/web1", raw_route(CLIENT)),
    ];
    let (url, records) = start_server(routes);
    let (snapshot, summary) = crawl_from_goiardi(&options(&url, dir_path)).unwrap();
    assert_eq!(snapshot.source.layout.as_deref(), Some("goiardi"));
    assert_eq!(snapshot.data.users[0].username, "admin");
    assert!(snapshot.data.users[0].admin);
    assert_eq!(snapshot.data.users[0].password_hash, "");
    assert_eq!(snapshot.data.clients[0].name, "web1");
    assert_eq!(snapshot.data.actor_keys[0].public_key, PUBLIC_KEY);
    assert_eq!(summary.clients, 1);
    let records = records.lock().unwrap();
    assert!(!records.is_empty());
    for record in records.iter() {
        assert_eq!(record.method, "GET");
        assert!(
            record.verified,
            "unsigned or invalid request {} {}: {:?}",
            record.method, record.path, record.verification_error
        );
    }
}

fn cookbooks_routes(with_blob: bool) -> (Vec<(&'static str, Route)>, String, Vec<u8>) {
    let (body, checksum, path) = blob_fixture("blob");
    let versions = json!([{"version": "1.2.3", "url": "/cookbooks/app/1.2.3"}]);
    // goiardi wraps both the list entry and the detail body under the
    // cookbook name; only the detail route carries every version.
    let cookbook = json!({"app": {"url": "/cookbooks/app", "versions": versions}});
    let version = json!({
        "name": "app-1.2.3",
        "cookbook": "app",
        "version": "1.2.3",
        "frozen?": false,
        "metadata": {"name": "app", "version": "1.2.3"},
        "recipes": [{"name": "default.rb", "path": "recipes/default.rb", "checksum": checksum}],
    });
    let mut routes = vec![
        ("/cookbooks", json_route(json!({"app": cookbook["app"]}))),
        ("/cookbooks/app", json_route(cookbook)),
        ("/cookbooks/app/1.2.3", json_route(version)),
    ];
    if with_blob {
        routes.push((path, bytes_route(200, body.clone())));
    }
    (routes, checksum, body)
}

#[test]
fn derives_a_missing_blob_url_from_base_and_copies_the_blob() {
    let out = tempdir();
    let bookshelf = tempdir();
    let (routes, checksum, body) = cookbooks_routes(true);
    let (url, _) = start_server(routes);
    let options = GoiardiOptions {
        bookshelf_dir: Some(bookshelf.path().to_path_buf()),
        ..options(&url, out.path())
    };
    let (snapshot, summary) = crawl_from_goiardi(&options).unwrap();
    assert_eq!(summary.cookbooks, 1);
    assert_eq!(summary.cookbook_versions, 1);
    assert_eq!(summary.checksums, 1);
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
    assert_eq!(snapshot.data.cookbooks[0].name, "app");

    let version = &snapshot.data.cookbook_versions[0];
    assert_eq!(version.name, "app");
    assert_eq!((version.major, version.minor, version.patch), (1, 2, 3));
    let raw = version.serialized_object.as_deref().unwrap();
    assert!(raw.contains(&checksum), "{raw}");
    assert_eq!(snapshot.data.checksums[0].checksum, checksum);
    assert_eq!(
        snapshot.data.cookbook_version_checksums[0].cookbook_version_id,
        version.id
    );
    assert_eq!(
        snapshot.data.cookbook_version_checksums[0].checksum,
        checksum
    );

    let org_id = &snapshot.data.orgs[0].id;
    let blob = bookshelf
        .path()
        .join(format!("organization-{org_id}/checksum-{checksum}"));
    assert_eq!(std::fs::read(blob).unwrap(), body);
}

#[test]
fn a_checksum_without_a_blob_is_an_error_when_blobs_are_requested() {
    let out = tempdir();
    let bookshelf = tempdir();
    let (routes, _, _) = cookbooks_routes(false);
    let (url, _) = start_server(routes);
    let options = GoiardiOptions {
        bookshelf_dir: Some(bookshelf.path().to_path_buf()),
        ..options(&url, out.path())
    };
    assert!(crawl_from_goiardi(&options).is_err());
}

#[test]
fn dry_run_writes_no_snapshot_and_fetches_no_blob() {
    let dir = tempdir();
    let bookshelf = tempdir();
    let output = dir.path().join("snapshot.json");
    let (routes, _, _) = cookbooks_routes(true);
    let (url, records) = start_server(routes);
    let mut options = options(&url, dir.path());
    options.bookshelf_dir = Some(bookshelf.path().to_path_buf());
    options.dry_run = true;
    let summary = from_goiardi_with_options(&options).unwrap();
    assert_eq!(summary.checksums, 1);
    assert!(!output.exists());
    assert!(records
        .lock()
        .unwrap()
        .iter()
        .all(|record| !record.path.starts_with("/file_store/")));
}
