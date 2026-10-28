mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_api::AppState;
use microwave_config::{Config, McpOrgConfig};
use microwave_crypto::SignVersion;
use microwave_store::Org;
use serde_json::{json, Value};
use tower::ServiceExt;

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
