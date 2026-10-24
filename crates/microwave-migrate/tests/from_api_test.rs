use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use microwave_crypto::{
    decode_signature, parse_sign_header, verify_request, AuthHeaders, CanonicalRequest,
};
use microwave_migrate::{
    crawl_from_api, from_api_with_options, import, FromApiOptions, MigrateError,
};
use microwave_store::{ActorType, Permission, SqliteStore};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const PUBLIC_KEY: &str = include_str!("../../microwave-crypto/tests/fixtures/test_key_pub.pem");
const PRIVATE_KEY: &str = include_str!("../../microwave-crypto/tests/fixtures/test_key.pem");
const CHECKSUM: &str = "0123456789abcdef0123456789abcdef";

const NODE_BODY: &str = r#"{"name":"web1","json_class":"Chef::Node","chef_type":"node","chef_environment":"_default","run_list":["role[web]"],"normal":{"tags":["blue"]}}"#;
const ROLE_BODY: &str = r#"{"name":"web","json_class":"Chef::Role","chef_type":"role","run_list":["recipe[app::default]"],"default_attributes":{},"override_attributes":{}}"#;
const ENVIRONMENT_BODY: &str = r#"{"name":"production","json_class":"Chef::Environment","chef_type":"environment","cookbook_versions":{},"default_attributes":{"tier":"prod"},"override_attributes":{}}"#;
const DATA_BAG_ITEM_BODY: &str =
    r#"{"id":"database","host":"db.example.com","password":"from-a-secret-store"}"#;
const COOKBOOK_BODY: &str = r#"{"name":"app-1.2.3","cookbook":"app","version":"1.2.3","frozen?":false,"metadata":{"name":"app","version":"1.2.3","long_description":"An app","dependencies":{"apt":">= 0.0.0"},"attributes":{}},"recipes":[{"name":"default.rb","path":"recipes/default.rb","checksum":"0123456789abcdef0123456789abcdef","specificity":"default"}],"chef_type":"cookbook_version","json_class":"Chef::CookbookVersion"}"#;
const POLICY_BODY: &str = r#"{"name":"app","revision_id":"rev1","run_list":["recipe[app::default]"],"cookbook_locks":{}}"#;

#[derive(Clone)]
struct AppState {
    routes: Arc<HashMap<String, Route>>,
    records: Arc<Mutex<Vec<Recorded>>>,
}

#[derive(Clone)]
struct Route {
    status: u16,
    body: String,
}

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    verified: bool,
    verification_error: Option<String>,
}

fn json_route(value: Value) -> Route {
    Route {
        status: 200,
        body: serde_json::to_string(&value).unwrap(),
    }
}

fn text_route(status: u16, body: impl Into<String>) -> Route {
    Route {
        status,
        body: body.into(),
    }
}

fn start_server(routes: Vec<(&str, Route)>) -> (String, Arc<Mutex<Vec<Recorded>>>) {
    let routes: HashMap<String, Route> = routes
        .into_iter()
        .map(|(path, route)| (path.to_string(), route))
        .collect();
    let records = Arc::new(Mutex::new(Vec::new()));
    let state = AppState {
        routes: Arc::new(routes),
        records: records.clone(),
    };
    let app = Router::new().fallback(handle).with_state(state);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            axum::serve(listener, app).await.unwrap();
        });
    });

    (format!("http://{address}"), records)
}

async fn handle(State(state): State<AppState>, request: Request) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let headers = request.headers().clone();
    let body = axum::body::to_bytes(request.into_body(), 10 * 1024 * 1024)
        .await
        .unwrap_or_default();

    let (verified, verification_error) = verify_signature(&method, &path, &headers, &body);
    let mut header_map = HashMap::new();
    for (name, value) in headers.iter() {
        header_map.insert(
            name.as_str().to_ascii_lowercase(),
            value.to_str().unwrap_or_default().to_string(),
        );
    }
    state.records.lock().unwrap().push(Recorded {
        method: method.to_string(),
        path: path.clone(),
        headers: header_map,
        verified,
        verification_error,
    });

    match state.routes.get(&path) {
        Some(route) => {
            let status = StatusCode::from_u16(route.status).unwrap();
            (
                status,
                [(header::CONTENT_TYPE, "application/json")],
                route.body.clone(),
            )
                .into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"not found"}"#.to_string(),
        )
            .into_response(),
    }
}

fn verify_signature(
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> (bool, Option<String>) {
    match verify_signature_inner(method, path, headers, body) {
        Ok(()) => (true, None),
        Err(error) => (false, Some(error)),
    }
}

fn verify_signature_inner(
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), String> {
    let sign = header(headers, "x-ops-sign")?;
    let version = parse_sign_header(&sign).map_err(|error| error.to_string())?;
    let user_id = header(headers, "x-ops-userid")?;
    let timestamp = header(headers, "x-ops-timestamp")?;
    let content_hash = header(headers, "x-ops-content-hash")?;
    let api_version = header(headers, "x-ops-server-api-version")?;

    let mut lines = Vec::new();
    for index in 1..=64 {
        match headers.get(format!("x-ops-authorization-{index}")) {
            Some(value) => lines.push(value.to_str().map_err(|error| error.to_string())?),
            None => break,
        }
    }
    let signature = decode_signature(&lines).map_err(|error| error.to_string())?;

    let auth = AuthHeaders {
        version,
        user_id: user_id.clone(),
        timestamp: timestamp.clone(),
        content_hash,
        signature,
    };
    let request = CanonicalRequest {
        method: method.as_str(),
        path,
        body,
        user_id: &user_id,
        timestamp: &timestamp,
        api_version: Some(&api_version),
        version,
    };
    verify_request(PUBLIC_KEY, &auth, &request).map_err(|error| error.to_string())
}

fn header(headers: &HeaderMap, name: &str) -> Result<String, String> {
    headers
        .get(name)
        .ok_or_else(|| format!("missing {name} header"))?
        .to_str()
        .map(str::to_string)
        .map_err(|error| error.to_string())
}

fn write_key(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("migrator.pem");
    std::fs::write(&path, PRIVATE_KEY).unwrap();
    path
}

fn options(url: &str, dir: &std::path::Path) -> FromApiOptions {
    FromApiOptions::new(url, "migrator", write_key(dir), dir.join("snapshot.json"))
}

fn acl(actors: &[&str], groups: &[&str]) -> Value {
    json!({
        "create": {"actors": actors, "groups": groups},
        "read": {"actors": actors, "groups": groups},
        "update": {"actors": actors, "groups": groups},
        "delete": {"actors": actors, "groups": groups},
        "grant": {"actors": actors, "groups": groups},
    })
}

fn full_routes() -> Vec<(&'static str, Route)> {
    vec![
        (
            "/organizations",
            json_route(json!({"acme": "/organizations/acme", "beta": "/organizations/beta"})),
        ),
        (
            "/organizations/acme",
            json_route(json!({
                "name": "acme",
                "full_name": "Acme Corp",
                "guid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            })),
        ),
        (
            "/organizations/beta",
            json_route(json!({
                "name": "beta",
                "full_name": "Beta Inc",
                "guid": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            })),
        ),
        (
            "/users",
            json_route(json!({
                "alice": "/users/alice",
                "bob": "/users/bob"
            })),
        ),
        (
            "/users/alice",
            json_route(json!({
                "username": "alice",
                "email": "alice@example.com",
                "display_name": "Alice",
                "first_name": "Alice",
                "middle_name": "",
                "last_name": "Liddell",
                "recovery_authentication_enabled": true,
                "external_authentication_uid": "alice-ldap"
            })),
        ),
        (
            "/users/bob",
            json_route(json!({
                "username": "bob",
                "email": "bob@example.com",
                "display_name": "Bob"
            })),
        ),
        (
            "/users/alice/keys",
            json_route(json!([
                {"name": "default", "uri": "/users/alice/keys/default", "expired": false}
            ])),
        ),
        (
            "/users/alice/keys/default",
            json_route(json!({
                "name": "default",
                "public_key": PUBLIC_KEY,
                "expiration_date": "infinity"
            })),
        ),
        ("/users/alice/_acl", json_route(acl(&["alice", "bob"], &[]))),
        (
            "/organizations/acme/users",
            json_route(json!({
                "alice": "/organizations/acme/users/alice",
                "bob": "/organizations/acme/users/bob"
            })),
        ),
        (
            "/organizations/acme/clients",
            json_route(json!({"web": "/organizations/acme/clients/web"})),
        ),
        (
            "/organizations/acme/clients/web",
            json_route(json!({
                "name": "web",
                "clientname": "web",
                "validator": false,
                "orgname": "acme",
                "public_key": PUBLIC_KEY
            })),
        ),
        (
            "/organizations/acme/clients/web/keys",
            json_route(json!([
                {
                    "name": "default",
                    "uri": "/organizations/acme/clients/web/keys/default",
                    "expired": false
                }
            ])),
        ),
        (
            "/organizations/acme/clients/web/keys/default",
            json_route(json!({
                "name": "default",
                "public_key": PUBLIC_KEY,
                "expiration_date": "infinity"
            })),
        ),
        (
            "/organizations/acme/clients/web/_acl",
            json_route(acl(&["web", "alice"], &["clients"])),
        ),
        (
            "/organizations/acme/groups",
            json_route(json!({
                "admins": "/organizations/acme/groups/admins",
                "clients": "/organizations/acme/groups/clients",
                "users": "/organizations/acme/groups/users"
            })),
        ),
        (
            "/organizations/acme/groups/admins",
            json_route(json!({
                "name": "admins",
                "groupname": "admins",
                "actors": ["alice"],
                "users": ["alice"],
                "clients": [],
                "groups": ["users"]
            })),
        ),
        (
            "/organizations/acme/groups/clients",
            json_route(json!({
                "name": "clients",
                "groupname": "clients",
                "actors": ["web"],
                "users": [],
                "clients": ["web"],
                "groups": []
            })),
        ),
        (
            "/organizations/acme/groups/users",
            json_route(json!({
                "name": "users",
                "groupname": "users",
                "actors": ["alice", "bob"],
                "users": ["alice", "bob"],
                "clients": [],
                "groups": []
            })),
        ),
        (
            "/organizations/acme/nodes",
            json_route(json!({"web1": "/organizations/acme/nodes/web1"})),
        ),
        ("/organizations/acme/nodes/web1", text_route(200, NODE_BODY)),
        (
            "/organizations/acme/nodes/web1/_acl",
            json_route(acl(&["alice", "bob"], &["admins"])),
        ),
        (
            "/organizations/acme/roles",
            json_route(json!({"web": "/organizations/acme/roles/web"})),
        ),
        ("/organizations/acme/roles/web", text_route(200, ROLE_BODY)),
        (
            "/organizations/acme/environments",
            json_route(json!({
                "_default": "/organizations/acme/environments/_default",
                "production": "/organizations/acme/environments/production"
            })),
        ),
        (
            "/organizations/acme/environments/_default",
            json_route(json!({
                "name": "_default",
                "json_class": "Chef::Environment",
                "chef_type": "environment",
                "cookbook_versions": {},
                "default_attributes": {},
                "override_attributes": {}
            })),
        ),
        (
            "/organizations/acme/environments/production",
            text_route(200, ENVIRONMENT_BODY),
        ),
        (
            "/organizations/acme/data",
            json_route(json!({"secrets": "/organizations/acme/data/secrets"})),
        ),
        (
            "/organizations/acme/data/secrets",
            json_route(json!({"database": "/organizations/acme/data/secrets/database"})),
        ),
        (
            "/organizations/acme/data/secrets/database",
            text_route(200, DATA_BAG_ITEM_BODY),
        ),
        (
            "/organizations/acme/cookbooks",
            json_route(json!({
                "app": {
                    "url": "/organizations/acme/cookbooks/app",
                    "versions": {
                        "1.2.3": [
                            {"version": "1.2.3", "url": "/organizations/acme/cookbooks/app/1.2.3"}
                        ]
                    }
                }
            })),
        ),
        (
            "/organizations/acme/cookbooks/app",
            json_route(json!({
                "url": "/organizations/acme/cookbooks/app",
                "versions": {
                    "1.2.3": [
                        {"version": "1.2.3", "url": "/organizations/acme/cookbooks/app/1.2.3"}
                    ]
                }
            })),
        ),
        (
            "/organizations/acme/cookbooks/app/1.2.3",
            text_route(200, COOKBOOK_BODY),
        ),
        (
            "/organizations/acme/policies",
            json_route(json!({
                "app": {
                    "uri": "/organizations/acme/policies/app",
                    "revisions": {"rev1": {}}
                }
            })),
        ),
        (
            "/organizations/acme/policies/app",
            json_route(json!({
                "name": "app",
                "revisions": {"rev1": {}}
            })),
        ),
        (
            "/organizations/acme/policies/app/revisions/rev1",
            text_route(200, POLICY_BODY),
        ),
        (
            "/organizations/acme/policy_groups",
            json_route(json!({
                "production": {
                    "uri": "/organizations/acme/policy_groups/production",
                    "policies": {"app": {"revision_id": "rev1"}}
                }
            })),
        ),
        (
            "/organizations/acme/policy_groups/production",
            json_route(json!({
                "name": "production",
                "policies": {"app": {"revision_id": "rev1"}}
            })),
        ),
        (
            "/organizations/acme/organizations/_acl",
            json_route(acl(&["alice"], &["admins"])),
        ),
    ]
}

#[test]
fn crawls_a_legacy_server_with_signed_requests_and_a_complete_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (url, records) = start_server(full_routes());
    let mut options = options(&url, dir.path());
    options.org = Some("acme".to_string());
    from_api_with_options(&options).unwrap();

    let snapshot_path = dir.path().join("snapshot.json");
    let text = std::fs::read_to_string(&snapshot_path).unwrap();
    let document: Value = serde_json::from_str(&text).unwrap();

    assert_eq!(document["format"], "microwave-server-snapshot");
    assert_eq!(document["version"], 1);
    assert_eq!(document["source"]["kind"], "legacy-api");
    assert_eq!(document["source"]["url"], url);
    assert!(document["source"]["data_dir"].is_null());

    let orgs = document["data"]["orgs"].as_array().unwrap();
    assert_eq!(orgs.len(), 1);
    assert_eq!(orgs[0]["name"], "acme");
    assert_eq!(orgs[0]["full_name"], "Acme Corp");
    assert_eq!(orgs[0]["id"], "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

    let users = document["data"]["users"].as_array().unwrap();
    assert_eq!(users.len(), 2);
    let alice = users
        .iter()
        .find(|user| user["username"] == "alice")
        .unwrap();
    assert_eq!(alice["email"], "alice@example.com");
    assert_eq!(alice["recovery_authentication_enabled"], true);
    assert_eq!(alice["external_authentication_uid"], "alice-ldap");
    assert_eq!(alice["password_hash"], "");
    assert_eq!(alice["password_salt"], "");
    assert!(!text.contains("BEGIN RSA PRIVATE KEY"));
    assert!(!text.contains("BEGIN PRIVATE KEY"));

    let clients = document["data"]["clients"].as_array().unwrap();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0]["name"], "web");
    assert_eq!(clients[0]["validator"], false);

    let keys = document["data"]["actor_keys"].as_array().unwrap();
    assert_eq!(keys.len(), 2);
    assert!(keys
        .iter()
        .any(|key| key["actor_type"] == "user" && key["public_key"] == PUBLIC_KEY));
    assert!(keys
        .iter()
        .any(|key| key["actor_type"] == "client" && key["public_key"] == PUBLIC_KEY));

    assert_eq!(
        document["data"]["org_user_associations"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let groups = document["data"]["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 3);
    assert!(document["data"]["authz_group_actors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["group_authz_id"] == groups[0]["authz_id"]
            && row["actor_authz_id"] == alice["authz_id"]));
    assert!(document["data"]["authz_group_groups"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["parent_group_authz_id"] == groups[0]["authz_id"]
            && row["child_group_authz_id"] == groups[2]["authz_id"]));

    let nodes = document["data"]["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0]["name"], "web1");
    assert_eq!(nodes[0]["environment"], "_default");
    assert_eq!(nodes[0]["serialized_object"], NODE_BODY);

    let roles = document["data"]["roles"].as_array().unwrap();
    assert_eq!(roles.len(), 1);
    assert_eq!(roles[0]["name"], "web");
    assert_eq!(roles[0]["serialized_object"], ROLE_BODY);

    let environments = document["data"]["environments"].as_array().unwrap();
    assert_eq!(environments.len(), 2);
    assert!(environments
        .iter()
        .any(|environment| environment["name"] == "production"));

    let data_bags = document["data"]["data_bags"].as_array().unwrap();
    assert_eq!(data_bags.len(), 1);
    assert_eq!(data_bags[0]["name"], "secrets");
    let bag_body: Value =
        serde_json::from_str(data_bags[0]["serialized_object"].as_str().unwrap()).unwrap();
    assert_eq!(bag_body["name"], "secrets");
    assert_eq!(bag_body["json_class"], "Chef::DataBag");
    assert_eq!(bag_body["chef_type"], "data_bag");
    let items = document["data"]["data_bag_items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["data_bag_name"], "secrets");
    assert_eq!(items[0]["item_name"], "database");
    assert_eq!(items[0]["serialized_object"], DATA_BAG_ITEM_BODY);

    let cookbooks = document["data"]["cookbooks"].as_array().unwrap();
    assert_eq!(cookbooks.len(), 1);
    assert_eq!(cookbooks[0]["name"], "app");
    let versions = document["data"]["cookbook_versions"].as_array().unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0]["name"], "app");
    assert_eq!(versions[0]["major"], 1);
    assert_eq!(versions[0]["minor"], 2);
    assert_eq!(versions[0]["patch"], 3);
    assert_eq!(versions[0]["frozen"], false);
    assert_eq!(versions[0]["serialized_object"], COOKBOOK_BODY);
    assert_eq!(
        versions[0]["metadata"],
        json!({
            "name": "app",
            "version": "1.2.3",
            "long_description": "An app",
            "dependencies": {"apt": ">= 0.0.0"},
            "attributes": {}
        })
        .to_string()
    );
    assert_eq!(document["data"]["checksums"][0]["checksum"], CHECKSUM);
    assert_eq!(
        document["data"]["cookbook_version_checksums"][0]["checksum"],
        CHECKSUM
    );

    let policies = document["data"]["policies"].as_array().unwrap();
    assert_eq!(policies.len(), 1);
    assert_eq!(policies[0]["name"], "app");
    let revisions = document["data"]["policy_revisions"].as_array().unwrap();
    assert_eq!(revisions.len(), 1);
    assert_eq!(revisions[0]["revision_id"], "rev1");
    assert_eq!(revisions[0]["serialized_object"], POLICY_BODY);
    let policy_groups = document["data"]["policy_groups"].as_array().unwrap();
    assert_eq!(policy_groups.len(), 1);
    assert_eq!(policy_groups[0]["name"], "production");
    let associations = document["data"]["policy_group_revision_associations"]
        .as_array()
        .unwrap();
    assert_eq!(associations.len(), 1);
    assert_eq!(associations[0]["policy_revision_name"], "app");
    assert_eq!(associations[0]["policy_revision_revision_id"], "rev1");

    let acl_actors = document["data"]["authz_acl_actor"].as_array().unwrap();
    assert!(acl_actors.iter().any(|row| {
        row["target_authz_id"] == nodes[0]["authz_id"]
            && row["permission"] == "read"
            && row["actor_authz_id"] == alice["authz_id"]
    }));
    assert!(acl_actors.iter().any(|row| {
        row["target_authz_id"] == nodes[0]["authz_id"]
            && row["permission"] == "update"
            && row["actor_authz_id"]
                == users.iter().find(|user| user["username"] == "bob").unwrap()["authz_id"]
    }));
    assert!(document["data"]["authz_acl_group"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["target_authz_id"] == nodes[0]["authz_id"]
            && row["permission"] == "read"
            && row["group_authz_id"] == groups[0]["authz_id"]));

    // The crawler output must be directly importable by the local importer.
    let target = tempfile::tempdir().unwrap();
    import(target.path(), &snapshot_path).unwrap();
    let store = SqliteStore::open(&target.path().join("microwave.db")).unwrap();
    let imported_org = store.find_org("acme").unwrap().unwrap();
    assert_eq!(imported_org.id, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let imported_node = store.find_node(&imported_org.id, "web1").unwrap().unwrap();
    assert_eq!(imported_node.serialized_object, NODE_BODY);
    assert_eq!(imported_node.environment, "_default");
    let alice_authz = alice["authz_id"].as_str().unwrap();
    let bob_authz = users.iter().find(|user| user["username"] == "bob").unwrap()["authz_id"]
        .as_str()
        .unwrap();
    assert!(store
        .check_permission(alice_authz, &imported_node.authz_id, Permission::Read)
        .unwrap());
    assert!(store
        .check_permission(bob_authz, &imported_node.authz_id, Permission::Update)
        .unwrap());
    assert!(store
        .find_client(&imported_org.id, "web")
        .unwrap()
        .is_some());
    assert!(store.find_role(&imported_org.id, "web").unwrap().is_some());
    assert!(store
        .find_environment(&imported_org.id, "production")
        .unwrap()
        .is_some());
    assert!(store
        .find_data_bag_item(&imported_org.id, "secrets", "database")
        .unwrap()
        .is_some());
    let imported_cookbook = store
        .find_cookbook_version(&imported_org.id, "app", "1.2.3")
        .unwrap()
        .unwrap();
    assert_eq!(imported_cookbook.checksums, vec![CHECKSUM.to_string()]);
    let imported_revision = store
        .find_policy_revision(&imported_org.id, "app", "rev1")
        .unwrap()
        .unwrap();
    assert_eq!(imported_revision.serialized_object, POLICY_BODY);
    assert!(store
        .find_policy_group_association(&imported_org.id, "production", "app")
        .unwrap()
        .is_some());
    assert!(store
        .find_actor_key(alice["id"].as_str().unwrap(), ActorType::User, "default")
        .unwrap()
        .is_some());

    let records = records.lock().unwrap();
    assert!(!records.is_empty());
    for record in records.iter() {
        assert!(
            record.verified,
            "unsigned or invalid request {} {}: {:?}",
            record.method, record.path, record.verification_error
        );
    }
    assert!(records
        .iter()
        .all(|record| record.headers["x-ops-userid"] == "migrator"));
    assert!(records.iter().all(|record| {
        record.headers["x-ops-sign"] == "algorithm=sha256;version=1.3;"
            && record.headers["x-ops-server-api-version"] == "2"
            && !record.headers["x-ops-content-hash"].is_empty()
            && record.headers.contains_key("x-ops-authorization-1")
            && !record.headers["x-ops-timestamp"].is_empty()
    }));
    assert!(records
        .iter()
        .any(|record| record.path == "/organizations/acme/nodes/web1"));
    // --org skips the organization list and every other organization.
    assert!(!records.iter().any(|record| record.path == "/organizations"));
    assert!(!records
        .iter()
        .any(|record| record.path.starts_with("/organizations/beta")));
}

#[test]
fn honours_the_requested_api_version() {
    let dir = tempfile::tempdir().unwrap();
    let (url, records) = start_server(vec![(
        "/organizations/acme",
        json_route(json!({"name": "acme", "full_name": "Acme"})),
    )]);
    let mut options = options(&url, dir.path());
    options.org = Some("acme".to_string());
    options.api_version = 1;
    from_api_with_options(&options).unwrap();

    let records = records.lock().unwrap();
    assert!(!records.is_empty());
    assert!(records
        .iter()
        .all(|record| record.verified && record.headers["x-ops-server-api-version"] == "1"));
}

#[test]
fn reports_authentication_failures_without_writing_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let (url, _records) = start_server(vec![("/organizations", text_route(401, ""))]);
    let options = options(&url, dir.path());
    let error = from_api_with_options(&options).unwrap_err();

    match error {
        MigrateError::Api {
            status, message, ..
        } => {
            assert_eq!(status, 401);
            assert!(message.contains("authentication failed"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(!dir.path().join("snapshot.json").exists());
}

#[test]
fn reports_a_missing_explicit_organization_without_writing_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let (url, _records) = start_server(Vec::new());
    let mut options = options(&url, dir.path());
    options.org = Some("missing".to_string());
    let error = from_api_with_options(&options).unwrap_err();

    match error {
        MigrateError::Api {
            status, message, ..
        } => {
            assert_eq!(status, 404);
            assert!(message.contains("missing"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(!dir.path().join("snapshot.json").exists());
}

#[test]
fn tolerates_missing_optional_collections() {
    let dir = tempfile::tempdir().unwrap();
    let (url, _records) = start_server(vec![
        (
            "/organizations",
            json_route(json!({"acme": "/organizations/acme"})),
        ),
        (
            "/organizations/acme",
            json_route(json!({"name": "acme", "full_name": "Acme Corp"})),
        ),
    ]);
    let options = options(&url, dir.path());
    let snapshot = crawl_from_api(&options).unwrap();

    assert_eq!(snapshot.data.orgs.len(), 1);
    assert_eq!(snapshot.data.orgs[0].name, "acme");
    assert!(snapshot.data.users.is_empty());
    assert!(snapshot.data.nodes.is_empty());
    assert!(snapshot.data.cookbooks.is_empty());
}

#[test]
fn follows_paginated_collections() {
    let dir = tempfile::tempdir().unwrap();
    let (url, records) = start_server(vec![
        (
            "/organizations",
            json_route(json!({
                "results": [{"name": "acme", "url": "/organizations/acme"}],
                "next": "/organizations/page2"
            })),
        ),
        (
            "/organizations/page2",
            json_route(json!({
                "results": [{"name": "beta", "url": "/organizations/beta"}],
                "next": null
            })),
        ),
        (
            "/organizations/acme",
            json_route(json!({"name": "acme", "full_name": "Acme Corp"})),
        ),
        (
            "/organizations/beta",
            json_route(json!({"name": "beta", "full_name": "Beta Inc"})),
        ),
    ]);
    let options = options(&url, dir.path());
    let snapshot = crawl_from_api(&options).unwrap();

    assert_eq!(
        snapshot
            .data
            .orgs
            .iter()
            .map(|org| org.name.as_str())
            .collect::<Vec<_>>(),
        vec!["acme", "beta"]
    );
    let records = records.lock().unwrap();
    assert!(records
        .iter()
        .any(|record| record.path == "/organizations/page2"));
    assert!(records.iter().all(|record| record.verified));
}

#[test]
fn rejects_an_invalid_url_before_sending_requests() {
    let dir = tempfile::tempdir().unwrap();
    let mut options = options("ftp://chef.example.com", dir.path());
    options.output = dir.path().join("snapshot.json");
    let error = from_api_with_options(&options).unwrap_err();
    assert!(matches!(error, MigrateError::InvalidUrl(_)));
    assert!(!options.output.exists());
}

#[test]
fn accepts_a_single_key_object_from_the_keys_api() {
    let dir = tempfile::tempdir().unwrap();
    let (url, _records) = start_server(vec![
        ("/users", json_route(json!({"alice": "/users/alice"}))),
        (
            "/users/alice",
            json_route(json!({"username": "alice", "email": "alice@example.com"})),
        ),
        (
            "/users/alice/keys",
            json_route(json!({
                "name": "default",
                "public_key": PUBLIC_KEY,
                "expiration_date": "infinity"
            })),
        ),
    ]);
    let options = options(&url, dir.path());
    let snapshot = crawl_from_api(&options).unwrap();

    assert_eq!(snapshot.data.actor_keys.len(), 1);
    assert_eq!(snapshot.data.actor_keys[0].actor_type, "user");
    assert_eq!(snapshot.data.actor_keys[0].key_name, "default");
    assert_eq!(snapshot.data.actor_keys[0].public_key, PUBLIC_KEY);
}

#[test]
fn accepts_an_inline_public_key_when_the_keys_api_is_absent() {
    let dir = tempfile::tempdir().unwrap();
    let (url, _records) = start_server(vec![
        ("/users", json_route(json!({"alice": "/users/alice"}))),
        (
            "/users/alice",
            json_route(json!({"username": "alice", "public_key": PUBLIC_KEY})),
        ),
    ]);
    let options = options(&url, dir.path());
    let snapshot = crawl_from_api(&options).unwrap();

    assert_eq!(snapshot.data.actor_keys.len(), 1);
    assert_eq!(snapshot.data.actor_keys[0].actor_type, "user");
    assert_eq!(snapshot.data.actor_keys[0].public_key, PUBLIC_KEY);
}
