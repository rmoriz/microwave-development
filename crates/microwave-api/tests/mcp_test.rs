mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_api::AppState;
use microwave_config::{Config, McpOrgConfig};
use microwave_crypto::SignVersion;
use microwave_objectstore::md5_hex;
use microwave_store::{NewNodeRun, Org, Permission};
use serde_json::{json, Value};
use std::io::Write;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;

fn mcp_config(orgs: &[(&str, &str)]) -> Config {
    let mut config = Config::default();
    config.mcp.enabled = true;
    for (org, token) in orgs {
        config.mcp.organizations.insert(
            (*org).to_string(),
            McpOrgConfig {
                token: Some((*token).to_string()),
                ..Default::default()
            },
        );
    }
    config
}

fn mcp_config_with_principal(org: &str, token: &str, principal: &str) -> Config {
    let mut config = mcp_config(&[(org, token)]);
    config
        .mcp
        .organizations
        .get_mut(org)
        .expect("test config contains the organization")
        .principal = Some(principal.to_string());
    config
}

/// One organization whose MCP entry has no inline token, so it can only
/// authenticate through the `mcp/<org>` secret store.
fn mcp_config_without_token(org: &str) -> Config {
    let mut config = Config::default();
    config.mcp.enabled = true;
    config.mcp.organizations.insert(
        org.to_string(),
        McpOrgConfig {
            token: None,
            ..Default::default()
        },
    );
    config
}

async fn mcp(app: &axum::Router, org: &str, token: &str, body: Value) -> axum::response::Response {
    mcp_with(app, org, Some(token), "localhost", body).await
}

async fn mcp_with(
    app: &axum::Router,
    org: &str,
    token: Option<&str>,
    host: &str,
    body: Value,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/organizations/{org}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("host", host);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    app.clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1.0" }
        }
    })
}

fn tools_list(id: u64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {} })
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn acme_app(config: Config) -> axum::Router {
    let state: AppState = support::state_with_config(config);
    support::add_org(&state, "acme").await;
    support::app(state)
}

async fn fixture(config: Config) -> (AppState, Org, axum::Router) {
    let state = support::state_with_config(config);
    let org = support::add_org(&state, "acme").await;
    let app = support::app(state.clone());
    (state, org, app)
}

async fn agent_fixture(config: Config) -> (AppState, Org, axum::Router) {
    let (state, org, app) = fixture(config).await;
    let agent = support::add_user(&state, "agent", "agent@example.com", false);
    support::associate(&state, &org, &agent);
    // Associating a user also adds them to the org's `users` group, which
    // grants read on every default container. Drop the user-specific group
    // membership so this principal really has no ACLs of its own.
    let group = state
        .store
        .find_group(&org.id, &agent.id)
        .await
        .unwrap()
        .expect("associating a user creates a user-specific group");
    state
        .store
        .remove_group_member(&group.authz_id, &agent.authz_id)
        .await
        .unwrap();
    (state, org, app)
}

async fn api(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &Value,
    user: &str,
) -> axum::response::Response {
    let body = body.to_string();
    app.clone()
        .oneshot(support::signed_request(
            method,
            path,
            body.as_bytes(),
            SignVersion::V1_3,
            user,
        ))
        .await
        .unwrap()
}

async fn create_node(app: &axum::Router, body: Value) {
    let response = api(app, "POST", "/organizations/acme/nodes", &body, "pivotal").await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn create_environment(app: &axum::Router, name: &str) {
    let body = json!({ "name": name });
    let response = api(
        app,
        "POST",
        "/organizations/acme/environments",
        &body,
        "pivotal",
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

/// Upload one cookbook version through the signed API. The checksum row and
/// blob are written directly so `validate_checksums` passes without a sandbox
/// round-trip (the checksum is the MD5 of `b"contents"`).
async fn upload_cookbook(state: &AppState, app: &axum::Router, org: &Org, version: &str) {
    let checksum = "98bf7d8c15784f0a3d63204441e1e2aa";
    state
        .store
        .ensure_checksums(&org.id, &[checksum.to_string()])
        .await
        .unwrap();
    state
        .object_store
        .put(&org.id, checksum, b"contents")
        .unwrap();
    let manifest = json!({
        "name": format!("apache-{version}"),
        "cookbook_name": "apache",
        "version": version,
        "json_class": "Chef::CookbookVersion",
        "chef_type": "cookbook_version",
        "frozen?": false,
        "metadata": { "version": version },
        "all_files": [{
            "name": "recipes/default.rb",
            "path": "recipes/default.rb",
            "checksum": checksum,
            "specificity": "default"
        }]
    });
    let path = format!("/organizations/acme/cookbooks/apache/{version}");
    let response = api(app, "PUT", &path, &manifest, "pivotal").await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn insert_run(
    state: &AppState,
    org: &Org,
    run_id: &str,
    status: &str,
    start_time: &str,
    error_count: i64,
    report: &Value,
) {
    let node_name = "web1";
    let report = report.to_string();
    state
        .store
        .store_node_run(
            &org.id,
            NewNodeRun {
                node_name,
                run_id,
                status,
                start_time: Some(start_time),
                end_time: Some(start_time),
                resource_count: 3,
                updated_resource_count: 1,
                error_count,
                report: &report,
            },
            50,
        )
        .await
        .unwrap();
}

/// Three runs for `web1`: two clean successes and a newer failed run with both
/// a top-level error and a resource error.
async fn seed_web1_runs(state: &AppState, org: &Org) {
    insert_run(
        state,
        org,
        "run-1",
        "success",
        "2026-10-27T10:00:00.000Z",
        0,
        &json!({
            "message_type": "run_converge",
            "node_name": "web1",
            "run_id": "run-1",
            "status": "success",
            "start_time": "2026-10-27T10:00:00.000Z",
            "end_time": "2026-10-27T10:01:00.000Z",
            "resources": [{ "type": "file", "name": "/etc/motd", "status": "updated" }]
        }),
    )
    .await;
    insert_run(
        state,
        org,
        "run-2",
        "success",
        "2026-10-27T11:00:00.000Z",
        0,
        &json!({
            "message_type": "run_converge",
            "node_name": "web1",
            "run_id": "run-2",
            "status": "success",
            "start_time": "2026-10-27T11:00:00.000Z",
            "end_time": "2026-10-27T11:01:00.000Z",
            "resources": [{ "type": "file", "name": "/etc/hosts", "status": "updated" }]
        }),
    )
    .await;
    insert_run(
        state,
        org,
        "run-3",
        "failure",
        "2026-10-27T12:00:00.000Z",
        2,
        &json!({
            "message_type": "run_converge",
            "node_name": "web1",
            "run_id": "run-3",
            "status": "failure",
            "start_time": "2026-10-27T12:00:00.000Z",
            "end_time": "2026-10-27T12:01:00.000Z",
            "error": {
                "class": "RuntimeError",
                "message": "converge failed",
                "backtrace": ["/var/chef/cache/cookbooks/apache/recipes/default.rb:1"],
                "description": {}
            },
            "resources": [
                { "type": "file", "name": "/etc/motd", "status": "updated" },
                {
                    "type": "service",
                    "name": "nginx",
                    "status": "failed",
                    "error": {
                        "class": "Chef::Exceptions::Service",
                        "message": "service nginx failed"
                    }
                }
            ]
        }),
    )
    .await;
}

async fn delete_node(app: &axum::Router, name: &str) {
    let response = api(
        app,
        "DELETE",
        &format!("/organizations/acme/nodes/{name}"),
        &json!({}),
        "pivotal",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

async fn call_tool_raw(
    app: &axum::Router,
    org: &str,
    token: &str,
    id: u64,
    name: &str,
    args: Value,
) -> Value {
    let response = mcp(
        app,
        org,
        token,
        json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": args }
        }),
    )
    .await;
    body_json(response).await
}

async fn call_tool(
    app: &axum::Router,
    org: &str,
    token: &str,
    id: u64,
    name: &str,
    args: Value,
) -> Value {
    let body = call_tool_raw(app, org, token, id, name, args).await;
    let text = body["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

#[tokio::test]
async fn mcp_endpoint_requires_a_token() {
    let app = acme_app(mcp_config(&[("acme", "a")])).await;

    let response = mcp_with(&app, "acme", None, "localhost", initialize()).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );
}

#[tokio::test]
async fn mcp_token_is_bound_to_its_org() {
    let state = support::state_with_config(mcp_config(&[("acme", "a"), ("beta", "b")]));
    support::add_org(&state, "acme").await;
    support::add_org(&state, "beta").await;
    let app = support::app(state);

    let response = mcp(&app, "beta", "a", initialize()).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );
}

#[tokio::test]
async fn secret_backed_token_authenticates() {
    let (state, _org, app) = fixture(mcp_config_without_token("acme")).await;
    state
        .store
        .set_secret("mcp", "acme", "s3cret")
        .await
        .unwrap();

    let response = mcp(&app, "acme", "s3cret", initialize()).await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn config_token_shadows_a_stored_secret() {
    let (state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    state.store.set_secret("mcp", "acme", "b").await.unwrap();

    let configured = mcp(&app, "acme", "a", initialize()).await;
    let secret = mcp(&app, "acme", "b", initialize()).await;

    assert_eq!(configured.status(), StatusCode::OK);
    assert_eq!(secret.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn removed_secret_no_longer_authenticates() {
    let (state, _org, app) = fixture(mcp_config_without_token("acme")).await;
    state
        .store
        .set_secret("mcp", "acme", "s3cret")
        .await
        .unwrap();
    state.store.remove_secret("mcp", "acme").await.unwrap();

    let response = mcp_with(&app, "acme", Some("s3cret"), "localhost", initialize()).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn entry_without_a_token_source_is_401() {
    let app = acme_app(mcp_config_without_token("acme")).await;

    let response = mcp_with(&app, "acme", Some("anything"), "localhost", initialize()).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response
        .headers()
        .get("www-authenticate")
        .and_then(|value| value.to_str().ok())
        .unwrap();
    assert!(challenge.starts_with("Bearer"), "{challenge}");
}

#[tokio::test]
async fn configured_principal_in_org_is_accepted() {
    let state = support::state_with_config(mcp_config_with_principal("acme", "a", "agent"));
    let org = support::add_org(&state, "acme").await;
    let user = support::add_user(&state, "agent", "agent@example.com", false);
    support::associate(&state, &org, &user);
    let app = support::app(state);

    let response = mcp(&app, "acme", "a", initialize()).await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn missing_configured_principal_is_500() {
    let app = acme_app(mcp_config_with_principal("acme", "a", "ghost")).await;

    let response = mcp(&app, "acme", "a", initialize()).await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn configured_principal_outside_org_is_500() {
    let state = support::state_with_config(mcp_config_with_principal("acme", "a", "agent"));
    support::add_org(&state, "acme").await;
    support::add_user(&state, "agent", "agent@example.com", false);
    let app = support::app(state);

    let response = mcp(&app, "acme", "a", initialize()).await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn unknown_or_unconfigured_org_is_404() {
    let state = support::state_with_config(mcp_config(&[("acme", "a")]));
    support::add_org(&state, "acme").await;
    support::add_org(&state, "beta").await;
    let app = support::app(state);

    let missing = mcp(&app, "missing", "a", initialize()).await;
    let unconfigured = mcp(&app, "beta", "a", initialize()).await;

    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(unconfigured.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn disabled_endpoint_is_404() {
    let state = support::state_with_config(Config::default());
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let response = mcp(&app, "acme", "a", initialize()).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn initialize_and_tools_list_work() {
    let app = acme_app(mcp_config(&[("acme", "a")])).await;

    let initialized = mcp(&app, "acme", "a", initialize()).await;
    assert_eq!(initialized.status(), StatusCode::OK);
    let initialized = body_json(initialized).await;
    assert!(initialized["result"]["serverInfo"].is_object());

    let listed = mcp(&app, "acme", "a", tools_list(2)).await;
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = body_json(listed).await;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for tool in [
        "list_nodes",
        "get_node",
        "list_environments",
        "get_environment",
        "list_cookbooks",
        "list_cookbook_versions",
        "get_cookbook_version",
        "list_runs",
        "latest_run",
        "get_run",
        "list_converge_errors",
    ] {
        assert!(names.contains(&tool), "missing tool {tool} in {names:?}");
    }
}

#[tokio::test]
async fn list_nodes_filters_by_query_and_environment() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    create_node(
        &app,
        json!({ "name": "web1", "chef_environment": "_default" }),
    )
    .await;
    create_node(&app, json!({ "name": "db1", "chef_environment": "prod" })).await;

    let all = call_tool(&app, "acme", "a", 2, "list_nodes", json!({})).await;
    let names: Vec<&str> = all["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|node| node["name"].as_str())
        .collect();
    assert_eq!(names, ["db1", "web1"]);
    assert_eq!(all["nodes"][1]["environment"], "_default");

    let filtered = call_tool(
        &app,
        "acme",
        "a",
        3,
        "list_nodes",
        json!({ "query": "web" }),
    )
    .await;
    assert_eq!(filtered["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["nodes"][0]["name"], "web1");

    let environment = call_tool(
        &app,
        "acme",
        "a",
        4,
        "list_nodes",
        json!({ "environment": "prod" }),
    )
    .await;
    assert_eq!(environment["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(environment["nodes"][0]["name"], "db1");
}

#[tokio::test]
async fn get_node_returns_the_full_object() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    create_node(
        &app,
        json!({
            "name": "web1",
            "chef_environment": "prod",
            "run_list": ["recipe[apache]"]
        }),
    )
    .await;

    let node = call_tool(&app, "acme", "a", 2, "get_node", json!({ "name": "web1" })).await;

    assert_eq!(node["name"], "web1");
    assert_eq!(node["chef_environment"], "prod");
    assert_eq!(node["run_list"], json!(["recipe[apache]"]));
}

#[tokio::test]
async fn list_and_get_environment_work() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    create_environment(&app, "prod").await;

    let listed = call_tool(&app, "acme", "a", 2, "list_environments", json!({})).await;
    let names: Vec<&str> = listed["environments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|environment| environment["name"].as_str())
        .collect();
    assert!(names.contains(&"_default"), "missing _default in {names:?}");
    assert!(names.contains(&"prod"), "missing prod in {names:?}");

    let environment = call_tool(
        &app,
        "acme",
        "a",
        3,
        "get_environment",
        json!({ "name": "_default" }),
    )
    .await;
    assert_eq!(environment["name"], "_default");
    assert_eq!(environment["chef_type"], "environment");
}

#[tokio::test]
async fn list_cookbooks_groups_versions_and_newest() {
    let (state, org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    upload_cookbook(&state, &app, &org, "1.0.0").await;
    upload_cookbook(&state, &app, &org, "2.0.0").await;

    let listed = call_tool(&app, "acme", "a", 2, "list_cookbooks", json!({})).await;
    let cookbooks = listed["cookbooks"].as_array().unwrap();
    assert_eq!(cookbooks.len(), 1);
    assert_eq!(cookbooks[0]["name"], "apache");
    assert_eq!(cookbooks[0]["newest_version"], "2.0.0");
    assert_eq!(cookbooks[0]["versions"], json!(["2.0.0", "1.0.0"]));

    let versions = call_tool(
        &app,
        "acme",
        "a",
        3,
        "list_cookbook_versions",
        json!({ "name": "apache" }),
    )
    .await;
    let versions = versions["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["version"], "2.0.0");
    assert_eq!(versions[1]["version"], "1.0.0");
    assert_eq!(versions[0]["frozen"], false);

    let manifest = call_tool(
        &app,
        "acme",
        "a",
        4,
        "get_cookbook_version",
        json!({ "name": "apache", "version": "1.0.0" }),
    )
    .await;
    assert_eq!(manifest["name"], "apache-1.0.0");
    assert_eq!(manifest["version"], "1.0.0");
    let files = manifest["all_files"].as_array().unwrap();
    assert_eq!(files[0]["name"], "recipes/default.rb");
    assert!(files[0]["url"].as_str().unwrap().contains("/checksums/"));

    let latest = call_tool(
        &app,
        "acme",
        "a",
        5,
        "get_cookbook_version",
        json!({ "name": "apache", "version": "_latest" }),
    )
    .await;
    assert_eq!(latest["name"], "apache-2.0.0");
}

#[tokio::test]
async fn read_tools_filter_objects_the_principal_cannot_read() {
    let (state, org, app) = agent_fixture(mcp_config_with_principal("acme", "a", "agent")).await;
    create_node(&app, json!({ "name": "web1" })).await;
    upload_cookbook(&state, &app, &org, "1.0.0").await;

    assert_eq!(
        call_tool(&app, "acme", "a", 2, "list_nodes", json!({})).await,
        json!({ "nodes": [] })
    );
    assert_eq!(
        call_tool(&app, "acme", "a", 3, "list_environments", json!({})).await,
        json!({ "environments": [] })
    );
    assert_eq!(
        call_tool(&app, "acme", "a", 4, "list_cookbooks", json!({})).await,
        json!({ "cookbooks": [] })
    );
    assert_eq!(
        call_tool(
            &app,
            "acme",
            "a",
            5,
            "list_cookbook_versions",
            json!({ "name": "apache" })
        )
        .await,
        json!({ "versions": [] })
    );

    for (id, tool, arguments) in [
        (6, "get_node", json!({ "name": "web1" })),
        (7, "get_environment", json!({ "name": "_default" })),
        (
            8,
            "get_cookbook_version",
            json!({ "name": "apache", "version": "1.0.0" }),
        ),
    ] {
        let result = call_tool_raw(&app, "acme", "a", id, tool, arguments).await;
        assert_eq!(result["result"]["isError"], json!(true), "{tool}");
        assert_eq!(
            result["result"]["content"][0]["text"], "forbidden",
            "{tool}"
        );
    }
}

#[tokio::test]
async fn scoped_principal_can_read_and_write_what_it_owns() {
    let (state, org, app) = agent_fixture(mcp_config_with_principal("acme", "a", "agent")).await;
    create_node(&app, json!({ "name": "web1" })).await;
    create_node(&app, json!({ "name": "web2" })).await;
    create_environment(&app, "prod").await;
    upload_cookbook(&state, &app, &org, "1.0.0").await;
    // A second cookbook the principal cannot read. It is created through the
    // same shared service the MCP upload tool calls, using the already
    // registered checksum.
    let nginx_manifest = json!({
        "name": "nginx-1.0.0",
        "cookbook_name": "nginx",
        "version": "1.0.0",
        "metadata": { "version": "1.0.0" },
        "all_files": [{
            "name": "recipes/default.rb",
            "path": "recipes/default.rb",
            "checksum": "98bf7d8c15784f0a3d63204441e1e2aa",
            "specificity": "default"
        }]
    });
    let response = api(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/nginx/1.0.0",
        &nginx_manifest,
        "pivotal",
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    seed_web1_runs(&state, &org).await;

    for (kind, name) in [
        ("nodes", "web1"),
        ("environments", "prod"),
        ("cookbooks", "apache"),
    ] {
        let authz_id = state
            .store
            .find_acl_target(&org.id, kind, name)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("missing authz id for {kind}/{name}"));
        state
            .sqlite()
            .set_acl_for_permission_split(
                &authz_id,
                Permission::Read,
                &["agent".to_string()],
                &[],
                &[],
            )
            .unwrap();
    }
    let nodes_container = state
        .store
        .find_acl_target(&org.id, "containers", "nodes")
        .await
        .unwrap()
        .unwrap();
    state
        .sqlite()
        .set_acl_for_permission_split(
            &nodes_container,
            Permission::Create,
            &["agent".to_string()],
            &[],
            &[],
        )
        .unwrap();

    let nodes = call_tool(&app, "acme", "a", 2, "list_nodes", json!({})).await;
    let names: Vec<&str> = nodes["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|node| node["name"].as_str())
        .collect();
    assert_eq!(names, ["web1"]);
    let node = call_tool(&app, "acme", "a", 3, "get_node", json!({ "name": "web1" })).await;
    assert_eq!(node["name"], "web1");
    let denied = call_tool_raw(&app, "acme", "a", 4, "get_node", json!({ "name": "web2" })).await;
    assert_eq!(denied["result"]["isError"], json!(true), "{denied}");
    assert_eq!(denied["result"]["content"][0]["text"], "forbidden");

    let environments = call_tool(&app, "acme", "a", 5, "list_environments", json!({})).await;
    let names: Vec<&str> = environments["environments"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|environment| environment["name"].as_str())
        .collect();
    assert_eq!(names, ["prod"]);
    let environment = call_tool(
        &app,
        "acme",
        "a",
        6,
        "get_environment",
        json!({ "name": "prod" }),
    )
    .await;
    assert_eq!(environment["name"], "prod");
    let denied = call_tool_raw(
        &app,
        "acme",
        "a",
        7,
        "get_environment",
        json!({ "name": "_default" }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], json!(true), "{denied}");
    assert_eq!(denied["result"]["content"][0]["text"], "forbidden");

    let cookbooks = call_tool(&app, "acme", "a", 8, "list_cookbooks", json!({})).await;
    let names: Vec<&str> = cookbooks["cookbooks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|cookbook| cookbook["name"].as_str())
        .collect();
    assert_eq!(names, ["apache"]);
    let versions = call_tool(
        &app,
        "acme",
        "a",
        9,
        "list_cookbook_versions",
        json!({ "name": "apache" }),
    )
    .await;
    assert!(!versions["versions"].as_array().unwrap().is_empty());
    let manifest = call_tool(
        &app,
        "acme",
        "a",
        10,
        "get_cookbook_version",
        json!({ "name": "apache", "version": "1.0.0" }),
    )
    .await;
    assert_eq!(manifest["name"], "apache-1.0.0");
    let denied = call_tool_raw(
        &app,
        "acme",
        "a",
        11,
        "get_cookbook_version",
        json!({ "name": "nginx", "version": "1.0.0" }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], json!(true), "{denied}");
    assert_eq!(denied["result"]["content"][0]["text"], "forbidden");

    let runs = call_tool(
        &app,
        "acme",
        "a",
        12,
        "list_runs",
        json!({ "node_name": "web1" }),
    )
    .await;
    let run_ids: Vec<&str> = runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|run| run["run_id"].as_str())
        .collect();
    assert_eq!(run_ids, ["run-3", "run-2", "run-1"]);
    let latest = call_tool(
        &app,
        "acme",
        "a",
        13,
        "latest_run",
        json!({ "node_name": "web1" }),
    )
    .await;
    assert_eq!(latest["run_id"], "run-3");
    let run = call_tool(
        &app,
        "acme",
        "a",
        14,
        "get_run",
        json!({ "node_name": "web1", "run_id": "run-3" }),
    )
    .await;
    assert_eq!(run["run_id"], "run-3");

    let created = call_tool(
        &app,
        "acme",
        "a",
        15,
        "create_node",
        json!({ "name": "web3" }),
    )
    .await;
    assert_eq!(created["name"], "web3");
    let node = call_tool(&app, "acme", "a", 16, "get_node", json!({ "name": "web3" })).await;
    assert_eq!(node["name"], "web3");
}

#[tokio::test]
async fn list_tools_clamp_to_max_results() {
    let mut config = mcp_config(&[("acme", "a")]);
    config.mcp.max_results = 2;
    let (_state, _org, app) = fixture(config).await;
    for name in ["web1", "web2", "web3"] {
        create_node(&app, json!({ "name": name })).await;
    }

    let nodes = call_tool(&app, "acme", "a", 2, "list_nodes", json!({})).await;

    assert_eq!(nodes["nodes"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn non_loopback_host_is_rejected_unless_configured() {
    let app = acme_app(mcp_config(&[("acme", "a")])).await;
    let rejected = mcp_with(&app, "acme", Some("a"), "evil.example.com", initialize()).await;
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

    let mut config = mcp_config(&[("acme", "a")]);
    config.mcp.allowed_hosts = vec!["evil.example.com".into()];
    let app = acme_app(config).await;
    let allowed = mcp_with(&app, "acme", Some("a"), "evil.example.com", initialize()).await;
    assert_eq!(allowed.status(), StatusCode::OK);
}

#[tokio::test]
async fn oversized_request_body_is_413() {
    let mut config = mcp_config(&[("acme", "a")]);
    config.mcp.max_upload_bytes = 1;
    let app = acme_app(config).await;
    let body = json!({ "padding": "x".repeat(2 * 1024 * 1024) });

    let response = mcp(&app, "acme", "a", body).await;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn list_runs_filters_and_orders() {
    let (state, org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    seed_web1_runs(&state, &org).await;

    let runs = call_tool(
        &app,
        "acme",
        "a",
        2,
        "list_runs",
        json!({ "node_name": "web1" }),
    )
    .await;
    let run_ids: Vec<&str> = runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|run| run["run_id"].as_str())
        .collect();
    assert_eq!(run_ids, ["run-3", "run-2", "run-1"]);
    assert_eq!(runs["runs"][0]["status"], "failure");
    assert_eq!(runs["runs"][0]["error_count"], 2);
    assert_eq!(runs["runs"][0]["start_time"], "2026-10-27T12:00:00.000Z");

    let failures = call_tool(
        &app,
        "acme",
        "a",
        3,
        "list_runs",
        json!({ "node_name": "web1", "failures_only": true }),
    )
    .await;
    let run_ids: Vec<&str> = failures["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|run| run["run_id"].as_str())
        .collect();
    assert_eq!(run_ids, ["run-3"]);

    let since = call_tool(
        &app,
        "acme",
        "a",
        4,
        "list_runs",
        json!({ "since": "2026-10-27T11:30:00Z" }),
    )
    .await;
    let run_ids: Vec<&str> = since["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|run| run["run_id"].as_str())
        .collect();
    assert_eq!(run_ids, ["run-3"]);
}

#[tokio::test]
async fn list_runs_filters_by_stored_status() {
    let (state, org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    seed_web1_runs(&state, &org).await;

    let failures = call_tool(
        &app,
        "acme",
        "a",
        2,
        "list_runs",
        json!({ "node_name": "web1", "status": "failure" }),
    )
    .await;

    let run_ids: Vec<&str> = failures["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|run| run["run_id"].as_str())
        .collect();
    assert_eq!(run_ids, ["run-3"]);
}

#[tokio::test]
async fn latest_run_includes_errors() {
    let (state, org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    seed_web1_runs(&state, &org).await;

    let latest = call_tool(
        &app,
        "acme",
        "a",
        2,
        "latest_run",
        json!({ "node_name": "web1" }),
    )
    .await;

    assert_eq!(latest["run_id"], "run-3");
    assert_eq!(latest["node_name"], "web1");
    assert_eq!(latest["error"]["class"], "RuntimeError");
    assert_eq!(latest["error"]["message"], "converge failed");
    let resource_errors = latest["resource_errors"].as_array().unwrap();
    assert_eq!(resource_errors.len(), 1);
    assert_eq!(resource_errors[0]["resource_type"], "service");
    assert_eq!(resource_errors[0]["resource_name"], "nginx");
    assert_eq!(
        resource_errors[0]["error_class"],
        "Chef::Exceptions::Service"
    );
    assert_eq!(resource_errors[0]["error_message"], "service nginx failed");
}

#[tokio::test]
async fn get_run_returns_the_full_report() {
    let (state, org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    seed_web1_runs(&state, &org).await;

    let run = call_tool(
        &app,
        "acme",
        "a",
        2,
        "get_run",
        json!({ "node_name": "web1", "run_id": "run-2" }),
    )
    .await;

    assert_eq!(run["run_id"], "run-2");
    assert_eq!(run["node_name"], "web1");
    let resources = run["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["name"], "/etc/hosts");
}

#[tokio::test]
async fn list_converge_errors_returns_top_level_and_resource_errors() {
    let (state, org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    seed_web1_runs(&state, &org).await;

    let result = call_tool(
        &app,
        "acme",
        "a",
        2,
        "list_converge_errors",
        json!({ "node_name": "web1" }),
    )
    .await;

    let errors = result["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 2);

    let top_level = errors
        .iter()
        .find(|error| error["resource_type"].is_null())
        .expect("top-level error entry");
    assert!(top_level["resource_name"].is_null());
    assert_eq!(top_level["run_id"], "run-3");
    assert_eq!(top_level["node_name"], "web1");
    assert_eq!(top_level["start_time"], "2026-10-27T12:00:00.000Z");
    assert_eq!(top_level["error_class"], "RuntimeError");
    assert_eq!(top_level["error_message"], "converge failed");

    let resource = errors
        .iter()
        .find(|error| !error["resource_type"].is_null())
        .expect("resource error entry");
    assert_eq!(resource["run_id"], "run-3");
    assert_eq!(resource["resource_type"], "service");
    assert_eq!(resource["resource_name"], "nginx");
    assert_eq!(resource["error_class"], "Chef::Exceptions::Service");
    assert_eq!(resource["error_message"], "service nginx failed");
}

#[tokio::test]
async fn list_converge_errors_does_not_hide_errors_behind_clean_runs() {
    let (state, org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    seed_web1_runs(&state, &org).await;
    // A clean run newer than the failed one. The limit must bound runs that
    // have errors, not the newest runs overall.
    insert_run(
        &state,
        &org,
        "run-4",
        "success",
        "2026-10-27T13:00:00.000Z",
        0,
        &json!({
            "message_type": "run_converge",
            "node_name": "web1",
            "run_id": "run-4",
            "status": "success",
            "start_time": "2026-10-27T13:00:00.000Z",
            "resources": []
        }),
    )
    .await;

    let result = call_tool(
        &app,
        "acme",
        "a",
        2,
        "list_converge_errors",
        json!({ "node_name": "web1", "limit": 1 }),
    )
    .await;

    let errors = result["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors.iter().all(|error| error["run_id"] == "run-3"));
}

#[tokio::test]
async fn invalid_since_is_an_invalid_argument_error() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;

    for (id, tool) in [(2, "list_runs"), (3, "list_converge_errors")] {
        let result =
            call_tool_raw(&app, "acme", "a", id, tool, json!({ "since": "yesterday" })).await;
        assert_eq!(result["result"]["isError"], json!(true), "{tool}");
        let message = result["result"]["content"][0]["text"].as_str().unwrap();
        assert!(message.contains("RFC 3339"), "{tool}: {message}");
    }
}

#[tokio::test]
async fn reports_of_deleted_nodes_are_hidden_from_non_superusers() {
    let state = support::state_with_config(mcp_config_with_principal("acme", "a", "agent"));
    let org = support::add_org(&state, "acme").await;
    let agent = support::add_user(&state, "agent", "agent@example.com", false);
    support::associate(&state, &org, &agent);
    let app = support::app(state.clone());

    create_node(&app, json!({ "name": "web1" })).await;
    seed_web1_runs(&state, &org).await;

    let before = call_tool(
        &app,
        "acme",
        "a",
        2,
        "list_runs",
        json!({ "node_name": "web1" }),
    )
    .await;
    assert_eq!(before["runs"].as_array().unwrap().len(), 3);

    delete_node(&app, "web1").await;

    assert_eq!(
        call_tool(&app, "acme", "a", 3, "list_runs", json!({})).await,
        json!({ "runs": [] })
    );
    let forbidden = call_tool_raw(
        &app,
        "acme",
        "a",
        4,
        "list_runs",
        json!({ "node_name": "web1" }),
    )
    .await;
    assert_eq!(forbidden["result"]["isError"], json!(true));
    assert_eq!(forbidden["result"]["content"][0]["text"], "forbidden");
}

#[tokio::test]
async fn reports_of_deleted_nodes_stay_visible_to_superusers() {
    let (state, org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    create_node(&app, json!({ "name": "web1" })).await;
    seed_web1_runs(&state, &org).await;
    delete_node(&app, "web1").await;

    let runs = call_tool(
        &app,
        "acme",
        "a",
        2,
        "list_runs",
        json!({ "node_name": "web1" }),
    )
    .await;

    assert_eq!(runs["runs"].as_array().unwrap().len(), 3);
    assert_eq!(runs["runs"][0]["run_id"], "run-3");
}

const WRITE_TOOLS: [&str; 8] = [
    "create_node",
    "update_node",
    "delete_node",
    "create_environment",
    "update_environment",
    "delete_environment",
    "upload_cookbook_version",
    "delete_cookbook_version",
];

async fn search_nodes(app: &axum::Router, query: &str) -> Value {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            &format!("/organizations/acme/search/node?q={query}"),
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    support::response_json(response).await
}

#[tokio::test]
async fn writes_false_hides_and_rejects_write_tools() {
    let mut config = mcp_config(&[("acme", "a")]);
    config.mcp.organizations.get_mut("acme").unwrap().writes = false;
    let app = acme_app(config).await;

    let listed = body_json(mcp(&app, "acme", "a", tools_list(2)).await).await;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for tool in WRITE_TOOLS {
        assert!(!names.contains(&tool), "unexpected write tool {tool}");
    }
    assert!(names.contains(&"get_node"));

    let result = call_tool_raw(
        &app,
        "acme",
        "a",
        3,
        "create_node",
        json!({ "name": "web1" }),
    )
    .await;
    let unknown_tool = result["error"]["message"].as_str().is_some_and(|message| {
        let message = message.to_lowercase();
        message.contains("unknown") || message.contains("not found")
    });
    assert!(
        unknown_tool || result["result"]["isError"] == json!(true),
        "{result}"
    );
}

#[tokio::test]
async fn create_update_and_delete_node() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;

    let created = call_tool(
        &app,
        "acme",
        "a",
        2,
        "create_node",
        json!({
            "name": "web1",
            "environment": "prod",
            "run_list": ["recipe[apache]"],
            "normal": { "platform": "ubuntu" }
        }),
    )
    .await;
    assert_eq!(created["name"], "web1");
    assert_eq!(created["environment"], "prod");
    assert!(created["run_list"].is_null(), "summary leaked the document");
    assert!(created["normal"].is_null(), "summary leaked the document");

    let node = call_tool(&app, "acme", "a", 3, "get_node", json!({ "name": "web1" })).await;
    assert_eq!(node["chef_environment"], "prod");
    assert_eq!(node["run_list"], json!(["recipe[apache]"]));
    assert_eq!(node["normal"]["platform"], "ubuntu");

    let updated = call_tool(
        &app,
        "acme",
        "a",
        4,
        "update_node",
        json!({
            "name": "web1",
            "node": {
                "name": "web1",
                "chef_environment": "prod",
                "run_list": ["role[base]"],
                "normal": { "platform": "debian" }
            }
        }),
    )
    .await;
    assert_eq!(updated["name"], "web1");
    assert_eq!(updated["environment"], "prod");

    let node = call_tool(&app, "acme", "a", 5, "get_node", json!({ "name": "web1" })).await;
    assert_eq!(node["run_list"], json!(["role[base]"]));
    assert_eq!(node["normal"]["platform"], "debian");

    let deleted = call_tool(
        &app,
        "acme",
        "a",
        6,
        "delete_node",
        json!({ "name": "web1" }),
    )
    .await;
    assert_eq!(deleted, json!({ "name": "web1", "deleted": true }));

    let missing = call_tool_raw(&app, "acme", "a", 7, "get_node", json!({ "name": "web1" })).await;
    assert_eq!(missing["result"]["isError"], json!(true));
    assert_eq!(missing["result"]["content"][0]["text"], "not found");
}

#[tokio::test]
async fn create_update_and_delete_environment() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;

    let created = call_tool(
        &app,
        "acme",
        "a",
        2,
        "create_environment",
        json!({
            "name": "prod",
            "attributes": {
                "default": { "app": { "port": 80 } },
                "override": { "app": { "port": 8080 } }
            },
            "cookbook_versions": { "apache": "= 1.0.0" }
        }),
    )
    .await;
    assert_eq!(created["name"], "prod");
    assert!(created["updated_at"].is_string());
    assert!(
        created["default_attributes"].is_null(),
        "summary leaked the document"
    );

    let environment = call_tool(
        &app,
        "acme",
        "a",
        3,
        "get_environment",
        json!({ "name": "prod" }),
    )
    .await;
    assert_eq!(environment["default_attributes"]["app"]["port"], 80);
    assert_eq!(environment["override_attributes"]["app"]["port"], 8080);
    assert_eq!(environment["cookbook_versions"]["apache"], "= 1.0.0");

    let updated = call_tool(
        &app,
        "acme",
        "a",
        4,
        "update_environment",
        json!({
            "name": "prod",
            "environment": {
                "name": "prod",
                "description": "production",
                "default_attributes": { "app": { "port": 81 } },
                "override_attributes": {},
                "cookbook_versions": {}
            }
        }),
    )
    .await;
    assert_eq!(updated["name"], "prod");
    assert_eq!(updated["renamed"], false);

    let environment = call_tool(
        &app,
        "acme",
        "a",
        5,
        "get_environment",
        json!({ "name": "prod" }),
    )
    .await;
    assert_eq!(environment["description"], "production");
    assert_eq!(environment["default_attributes"]["app"]["port"], 81);

    let renamed = call_tool(
        &app,
        "acme",
        "a",
        6,
        "update_environment",
        json!({
            "name": "prod",
            "new_name": "staging",
            "environment": {
                "name": "prod",
                "description": "staging",
                "default_attributes": {},
                "override_attributes": {},
                "cookbook_versions": {}
            }
        }),
    )
    .await;
    assert_eq!(renamed["name"], "staging");
    assert_eq!(renamed["renamed"], true);

    let environment = call_tool(
        &app,
        "acme",
        "a",
        7,
        "get_environment",
        json!({ "name": "staging" }),
    )
    .await;
    assert_eq!(environment["description"], "staging");

    let deleted = call_tool(
        &app,
        "acme",
        "a",
        8,
        "delete_environment",
        json!({ "name": "staging" }),
    )
    .await;
    assert_eq!(deleted, json!({ "name": "staging", "deleted": true }));

    let default = call_tool_raw(
        &app,
        "acme",
        "a",
        9,
        "delete_environment",
        json!({ "name": "_default" }),
    )
    .await;
    assert_eq!(default["result"]["isError"], json!(true));
    let message = default["result"]["content"][0]["text"].as_str().unwrap();
    assert!(message.contains("_default"), "{message}");
}

#[tokio::test]
async fn create_environment_accepts_only_typed_attributes() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;

    let created = call_tool(
        &app,
        "acme",
        "a",
        2,
        "create_environment",
        json!({
            "name": "prod",
            "attributes": {
                "default": { "app": { "port": 80 } },
                "override": { "x": true }
            }
        }),
    )
    .await;
    assert_eq!(created["name"], "prod");

    let environment = call_tool(
        &app,
        "acme",
        "a",
        3,
        "get_environment",
        json!({ "name": "prod" }),
    )
    .await;
    assert_eq!(environment["default_attributes"]["app"]["port"], 80);
    assert_eq!(environment["override_attributes"]["x"], true);

    let invalid = call_tool_raw(
        &app,
        "acme",
        "a",
        4,
        "create_environment",
        json!({
            "name": "staging",
            "attributes": { "defaults": {} }
        }),
    )
    .await;
    let rejected = invalid["error"].is_object() || invalid["result"]["isError"] == json!(true);
    assert!(rejected, "{invalid}");
}

#[tokio::test]
async fn write_acl_denial_is_reported() {
    let (_state, _org, app) = agent_fixture(mcp_config_with_principal("acme", "a", "agent")).await;

    let result = call_tool_raw(
        &app,
        "acme",
        "a",
        2,
        "create_node",
        json!({ "name": "web1" }),
    )
    .await;

    assert_eq!(result["result"]["isError"], json!(true), "{result}");
    assert_eq!(result["result"]["content"][0]["text"], "forbidden");
}

#[tokio::test]
async fn write_changes_update_the_search_index() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;

    call_tool(
        &app,
        "acme",
        "a",
        2,
        "create_node",
        json!({ "name": "web1", "environment": "prod" }),
    )
    .await;

    let hits = search_nodes(&app, "name:web1").await;
    assert_eq!(hits["total"], 1, "{hits}");
    assert_eq!(hits["rows"][0]["name"], "web1");

    call_tool(
        &app,
        "acme",
        "a",
        3,
        "delete_node",
        json!({ "name": "web1" }),
    )
    .await;

    let hits = search_nodes(&app, "name:web1").await;
    assert_eq!(hits["total"], 0, "{hits}");
}

#[tokio::test]
async fn oversized_get_run_is_an_error_not_truncated() {
    let mut config = mcp_config(&[("acme", "a")]);
    config.mcp.max_result_bytes = 1024;
    let (state, org, app) = fixture(config).await;
    insert_run(
        &state,
        &org,
        "big-run",
        "success",
        "2026-10-27T13:00:00.000Z",
        0,
        &json!({
            "message_type": "run_converge",
            "node_name": "web1",
            "run_id": "big-run",
            "status": "success",
            "start_time": "2026-10-27T13:00:00.000Z",
            "padding": "x".repeat(2048)
        }),
    )
    .await;

    let result = call_tool_raw(
        &app,
        "acme",
        "a",
        2,
        "get_run",
        json!({ "node_name": "web1", "run_id": "big-run" }),
    )
    .await;

    assert_eq!(result["result"]["isError"], json!(true));
    assert_eq!(
        result["result"]["content"][0]["text"],
        "result exceeds max_result_bytes"
    );
}

fn upload_args(name: &str, version: &str, files: Value) -> Value {
    json!({
        "name": name,
        "version": version,
        "metadata": {},
        "files": files,
    })
}

fn inline_file(path: &str, content_base64: &str) -> Value {
    json!({ "path": path, "content_base64": content_base64 })
}

#[tokio::test]
async fn upload_cookbook_version_with_inline_files() {
    let (state, org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    let recipe = "cookbook contents";
    let template = "server {}";

    let uploaded = call_tool(
        &app,
        "acme",
        "a",
        2,
        "upload_cookbook_version",
        upload_args(
            "nginx",
            "1.2.0",
            json!([
                inline_file("recipes/default.rb", "Y29va2Jvb2sgY29udGVudHM="),
                {
                    "path": "templates/default/nginx.conf.erb",
                    "content_base64": "c2VydmVyIHt9",
                    "specificity": "default"
                }
            ]),
        ),
    )
    .await;
    assert_eq!(
        uploaded,
        json!({ "name": "nginx", "version": "1.2.0", "frozen": false })
    );

    let manifest = call_tool(
        &app,
        "acme",
        "a",
        3,
        "get_cookbook_version",
        json!({ "name": "nginx", "version": "1.2.0" }),
    )
    .await;
    assert_eq!(manifest["name"], "nginx-1.2.0");
    assert_eq!(manifest["cookbook_name"], "nginx");
    assert_eq!(manifest["version"], "1.2.0");
    let paths: Vec<&str> = manifest["all_files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|file| file["path"].as_str())
        .collect();
    assert!(paths.contains(&"recipes/default.rb"), "{paths:?}");
    assert!(
        paths.contains(&"templates/default/nginx.conf.erb"),
        "{paths:?}"
    );

    assert_eq!(
        state
            .object_store
            .get(&org.id, &md5_hex(recipe.as_bytes()))
            .unwrap(),
        recipe.as_bytes()
    );
    assert_eq!(
        state
            .object_store
            .get(&org.id, &md5_hex(template.as_bytes()))
            .unwrap(),
        template.as_bytes()
    );
}

#[tokio::test]
async fn upload_accepts_an_existing_checksum() {
    let (state, org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    let checksum = md5_hex(b"hello");
    state
        .store
        .ensure_checksums(&org.id, std::slice::from_ref(&checksum))
        .await
        .unwrap();
    state
        .object_store
        .put(&org.id, &checksum, b"hello")
        .unwrap();

    let uploaded = call_tool(
        &app,
        "acme",
        "a",
        2,
        "upload_cookbook_version",
        upload_args(
            "nginx",
            "1.0.0",
            json!([{ "path": "recipes/default.rb", "checksum": checksum }]),
        ),
    )
    .await;
    assert_eq!(
        uploaded,
        json!({ "name": "nginx", "version": "1.0.0", "frozen": false })
    );

    let manifest = call_tool(
        &app,
        "acme",
        "a",
        3,
        "get_cookbook_version",
        json!({ "name": "nginx", "version": "1.0.0" }),
    )
    .await;
    let paths: Vec<&str> = manifest["all_files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|file| file["path"].as_str())
        .collect();
    assert_eq!(paths, ["recipes/default.rb"]);
}

#[tokio::test]
async fn upload_rejects_invalid_and_unknown_checksums() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;

    let invalid = call_tool_raw(
        &app,
        "acme",
        "a",
        2,
        "upload_cookbook_version",
        upload_args(
            "nginx",
            "1.0.0",
            json!([{ "path": "recipes/default.rb", "checksum": "NOTHEX" }]),
        ),
    )
    .await;
    assert_eq!(invalid["result"]["isError"], json!(true), "{invalid}");
    let message = invalid["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_lowercase();
    assert!(message.contains("checksum"), "{message}");

    let missing = call_tool_raw(
        &app,
        "acme",
        "a",
        3,
        "upload_cookbook_version",
        upload_args(
            "nginx",
            "1.0.0",
            json!([{
                "path": "recipes/default.rb",
                "checksum": "0123456789abcdef0123456789abcdef"
            }]),
        ),
    )
    .await;
    assert_eq!(missing["result"]["isError"], json!(true), "{missing}");
    let message = missing["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_lowercase();
    assert!(message.contains("not been uploaded"), "{message}");
}

#[tokio::test]
async fn upload_rejects_missing_base64_and_oversized_payloads() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;

    let invalid = call_tool_raw(
        &app,
        "acme",
        "a",
        2,
        "upload_cookbook_version",
        upload_args(
            "nginx",
            "1.0.0",
            json!([inline_file("recipes/default.rb", "!!!")]),
        ),
    )
    .await;
    assert_eq!(invalid["result"]["isError"], json!(true), "{invalid}");
    let message = invalid["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_lowercase();
    assert!(message.contains("base64"), "{message}");

    let missing = call_tool_raw(
        &app,
        "acme",
        "a",
        3,
        "upload_cookbook_version",
        upload_args("nginx", "1.0.0", json!([{ "path": "recipes/default.rb" }])),
    )
    .await;
    assert_eq!(missing["result"]["isError"], json!(true), "{missing}");
    let message = missing["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .to_lowercase();
    assert!(message.contains("content_base64"), "{message}");

    let mut config = mcp_config(&[("acme", "a")]);
    config.mcp.max_upload_bytes = 64;
    let (_state, _org, app) = fixture(config).await;
    let forty_bytes = "eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eHh4eA==";
    let oversized = call_tool_raw(
        &app,
        "acme",
        "a",
        4,
        "upload_cookbook_version",
        upload_args(
            "nginx",
            "1.0.0",
            json!([
                inline_file("recipes/default.rb", forty_bytes),
                inline_file("recipes/other.rb", forty_bytes)
            ]),
        ),
    )
    .await;
    assert_eq!(oversized["result"]["isError"], json!(true), "{oversized}");
    let message = oversized["result"]["content"][0]["text"].as_str().unwrap();
    assert!(message.contains("max_upload_bytes"), "{message}");

    let too_many: Vec<Value> = (0..101)
        .map(|index| inline_file(&format!("recipes/{index}.rb"), ""))
        .collect();
    let rejected = call_tool_raw(
        &app,
        "acme",
        "a",
        5,
        "upload_cookbook_version",
        upload_args("nginx", "1.0.0", Value::Array(too_many)),
    )
    .await;
    assert_eq!(rejected["result"]["isError"], json!(true), "{rejected}");
    let message = rejected["result"]["content"][0]["text"].as_str().unwrap();
    assert!(message.contains("100"), "{message}");
}

#[tokio::test]
async fn upload_honors_frozen_and_force() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    let files = json!([inline_file(
        "recipes/default.rb",
        "Y29va2Jvb2sgY29udGVudHM="
    )]);

    let mut args = upload_args("nginx", "1.0.0", files.clone());
    args["frozen"] = json!(true);
    let created = call_tool(&app, "acme", "a", 2, "upload_cookbook_version", args).await;
    assert_eq!(created["frozen"], true);

    let denied = call_tool_raw(
        &app,
        "acme",
        "a",
        3,
        "upload_cookbook_version",
        upload_args("nginx", "1.0.0", files.clone()),
    )
    .await;
    assert_eq!(denied["result"]["isError"], json!(true), "{denied}");
    let message = denied["result"]["content"][0]["text"].as_str().unwrap();
    assert!(message.contains("frozen"), "{message}");
    assert!(message.contains("force"), "{message}");

    let mut forced_args = upload_args("nginx", "1.0.0", files);
    forced_args["force"] = json!(true);
    let forced = call_tool(&app, "acme", "a", 4, "upload_cookbook_version", forced_args).await;
    assert_eq!(forced["frozen"], true);
}

#[tokio::test]
async fn delete_cookbook_version_reports_the_resolved_version() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;

    for (id, version) in [(2, "1.0.0"), (3, "2.0.0")] {
        call_tool(
            &app,
            "acme",
            "a",
            id,
            "upload_cookbook_version",
            upload_args(
                "nginx",
                version,
                json!([inline_file(
                    "recipes/default.rb",
                    "Y29va2Jvb2sgY29udGVudHM="
                )]),
            ),
        )
        .await;
    }

    let deleted = call_tool(
        &app,
        "acme",
        "a",
        4,
        "delete_cookbook_version",
        json!({ "name": "nginx", "version": "latest" }),
    )
    .await;
    assert_eq!(
        deleted,
        json!({ "name": "nginx", "version": "2.0.0", "deleted": true })
    );
}

#[tokio::test]
async fn delete_cookbook_version_falls_back_to_metadata_version() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;
    // A manifest without a top-level `version` is accepted by the HTTP API as
    // long as `metadata.version` matches the URL.
    let manifest = json!({
        "name": "nginx-1.0.0",
        "cookbook_name": "nginx",
        "metadata": { "version": "1.0.0" },
        "all_files": []
    });
    let response = api(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/nginx/1.0.0",
        &manifest,
        "pivotal",
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let deleted = call_tool(
        &app,
        "acme",
        "a",
        2,
        "delete_cookbook_version",
        json!({ "name": "nginx", "version": "latest" }),
    )
    .await;

    assert_eq!(
        deleted,
        json!({ "name": "nginx", "version": "1.0.0", "deleted": true })
    );
}

#[tokio::test]
async fn delete_cookbook_version_removes_the_version() {
    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;

    call_tool(
        &app,
        "acme",
        "a",
        2,
        "upload_cookbook_version",
        upload_args(
            "nginx",
            "1.0.0",
            json!([inline_file(
                "recipes/default.rb",
                "Y29va2Jvb2sgY29udGVudHM="
            )]),
        ),
    )
    .await;

    let deleted = call_tool(
        &app,
        "acme",
        "a",
        3,
        "delete_cookbook_version",
        json!({ "name": "nginx", "version": "1.0.0" }),
    )
    .await;
    assert_eq!(
        deleted,
        json!({ "name": "nginx", "version": "1.0.0", "deleted": true })
    );

    let missing = call_tool_raw(
        &app,
        "acme",
        "a",
        4,
        "get_cookbook_version",
        json!({ "name": "nginx", "version": "1.0.0" }),
    )
    .await;
    assert_eq!(missing["result"]["isError"], json!(true), "{missing}");
    assert_eq!(missing["result"]["content"][0]["text"], "not found");
}

#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl Write for LogCapture {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The homepage promises one structured line per call, with the target on
/// writes. Pin the fields so a rename or a dropped write target fails here.
#[tokio::test]
async fn tool_calls_emit_one_structured_log_line_with_the_target() {
    let capture = LogCapture::default();
    let writer = capture.clone();
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .try_init()
        .ok();

    let (_state, _org, app) = fixture(mcp_config(&[("acme", "a")])).await;

    call_tool(&app, "acme", "a", 2, "list_nodes", json!({})).await;
    call_tool(
        &app,
        "acme",
        "a",
        3,
        "create_node",
        json!({ "name": "log-probe" }),
    )
    .await;

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();

    let read = logs
        .lines()
        .find(|line| line.contains("mcp tool call") && line.contains("tool=\"list_nodes\""))
        .unwrap_or_else(|| panic!("read log line missing from {logs:?}"));
    assert!(read.contains("organization=acme"), "log line: {read}");
    // Parallel tests share this process-wide subscriber, so the principal may
    // belong to another test. The field must still be present.
    assert!(read.contains("principal="), "log line: {read}");
    assert!(read.contains("duration_ms="), "log line: {read}");
    assert!(read.contains("result_count="), "log line: {read}");
    assert!(read.contains("outcome=\"ok\""), "log line: {read}");
    assert!(!read.contains("target="), "log line: {read}");

    let write = logs
        .lines()
        .find(|line| line.contains("mcp tool call") && line.contains("tool=\"create_node\""))
        .unwrap_or_else(|| panic!("write log line missing from {logs:?}"));
    assert!(write.contains("target=\"log-probe\""), "log line: {write}");
}
