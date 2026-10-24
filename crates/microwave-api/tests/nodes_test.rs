mod support;

use axum::http::StatusCode;
use axum::response::Response;
use microwave_crypto::SignVersion;
use serde_json::json;
use tower::ServiceExt;

async fn create_org(app: &axum::Router) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations",
            br#"{"name":"acme","full_name":"Acme Corp"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn request(app: &axum::Router, method: &str, path: &str, body: &[u8]) -> Response {
    app.clone()
        .oneshot(support::signed_request(
            method,
            path,
            body,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap()
}

async fn create_node(app: &axum::Router, body: &[u8]) -> Response {
    request(app, "POST", "/organizations/acme/nodes", body).await
}

async fn get_node(app: &axum::Router, name: &str) -> Response {
    request(
        app,
        "GET",
        &format!("/organizations/acme/nodes/{name}"),
        b"",
    )
    .await
}

async fn delete_node(app: &axum::Router, name: &str) -> Response {
    request(
        app,
        "DELETE",
        &format!("/organizations/acme/nodes/{name}"),
        b"",
    )
    .await
}

async fn put_node(app: &axum::Router, name: &str, body: &[u8]) -> Response {
    request(
        app,
        "PUT",
        &format!("/organizations/acme/nodes/{name}"),
        body,
    )
    .await
}

async fn list_environment_nodes(app: &axum::Router, environment: &str) -> Response {
    request(
        app,
        "GET",
        &format!("/organizations/acme/environments/{environment}/nodes"),
        b"",
    )
    .await
}

async fn assert_error(response: Response, status: StatusCode, message: &str) {
    assert_eq!(response.status(), status);
    let body = support::response_json(response).await;
    assert_eq!(body, json!({ "error": [message] }));
}

#[tokio::test]
async fn creates_lists_reads_updates_and_deletes_a_node() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let node = br#"{"name":"web1","run_list":["recipe[apache]"],"normal":{"role":"web"}}"#;
    let response = create_node(&app, node).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = support::response_json(response).await;
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/organizations/acme/nodes/web1"
    );

    let response = request(&app, "GET", "/organizations/acme/nodes", b"").await;
    assert_eq!(response.status(), StatusCode::OK);
    let listed = support::response_json(response).await;
    assert_eq!(
        listed["web1"],
        "http://0.0.0.0:8000/organizations/acme/nodes/web1"
    );

    let response = get_node(&app, "web1").await;
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = support::response_json(response).await;
    assert_eq!(fetched["name"], "web1");
    assert_eq!(fetched["chef_environment"], "_default");
    assert_eq!(fetched["json_class"], "Chef::Node");
    assert_eq!(fetched["chef_type"], "node");
    assert_eq!(fetched["run_list"], json!(["recipe[apache]"]));

    let updated = br#"{"name":"web1","chef_environment":"production","run_list":[]}"#;
    let response = put_node(&app, "web1", updated).await;
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = support::response_json(response).await;
    assert_eq!(fetched["chef_environment"], "production");

    let response = delete_node(&app, "web1").await;
    assert_eq!(response.status(), StatusCode::OK);
    let deleted = support::response_json(response).await;
    assert_eq!(deleted["name"], "web1");
    assert_eq!(deleted["json_class"], "Chef::Node");
    assert_eq!(deleted["chef_type"], "node");
    assert_eq!(deleted["chef_environment"], "production");
    assert_eq!(deleted["run_list"], json!([]));

    let response = get_node(&app, "web1").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_defaults_and_normalizes_node_payload() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let node = br#"{"name":"web1","run_list":["base","recipe[base]","role[web]"]}"#;
    let response = create_node(&app, node).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let fetched = support::response_json(get_node(&app, "web1").await).await;
    assert_eq!(
        fetched,
        json!({
            "name": "web1",
            "chef_environment": "_default",
            "json_class": "Chef::Node",
            "chef_type": "node",
            "normal": {},
            "default": {},
            "override": {},
            "automatic": {},
            "run_list": ["recipe[base]", "role[web]"]
        })
    );
}

#[tokio::test]
async fn create_rejects_missing_name() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = create_node(&app, br#"{"run_list":[]}"#).await;
    assert_error(response, StatusCode::BAD_REQUEST, "Field 'name' missing").await;
}

#[tokio::test]
async fn create_rejects_invalid_name() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = create_node(&app, br#"{"name":"node@127.0.0.1"}"#).await;
    assert_error(response, StatusCode::BAD_REQUEST, "Field 'name' invalid").await;
}

#[tokio::test]
async fn create_rejects_invalid_environment() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = create_node(
        &app,
        br#"{"name":"web1","chef_environment":"pedant:no_colon_in_environment_name"}"#,
    )
    .await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "Field 'chef_environment' invalid",
    )
    .await;
}

#[tokio::test]
async fn create_rejects_invalid_json_class_and_chef_type() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = create_node(&app, br#"{"name":"web1","json_class":"anything_else"}"#).await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "Field 'json_class' invalid",
    )
    .await;

    let response = create_node(&app, br#"{"name":"web2","chef_type":"anything_else"}"#).await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "Field 'chef_type' invalid",
    )
    .await;
}

#[tokio::test]
async fn create_rejects_unknown_top_level_keys() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = create_node(
        &app,
        br#"{"name":"web1","something_random":"something random"}"#,
    )
    .await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "Invalid key something_random in request body",
    )
    .await;
}

#[tokio::test]
async fn create_rejects_invalid_run_list() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    for body in [
        r#"{"name":"web1","run_list":"string"}"#,
        r#"{"name":"web1","run_list":["gibberish@1"]}"#,
        r#"{"name":"web1","run_list":["node[gibberish]"]}"#,
    ] {
        let response = create_node(&app, body.as_bytes()).await;
        assert_error(
            response,
            StatusCode::BAD_REQUEST,
            "Field 'run_list' is not a valid run list",
        )
        .await;
    }
}

#[tokio::test]
async fn create_rejects_non_hash_attributes() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = create_node(&app, br#"{"name":"web1","normal":1}"#).await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "Field 'normal' is not a hash",
    )
    .await;
}

#[tokio::test]
async fn create_rejects_invalid_policy_name_and_group() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = create_node(&app, br#"{"name":"web1","policy_name":42}"#).await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "Field 'policy_name' invalid",
    )
    .await;

    let long = "F".repeat(256);
    let body = json!({ "name": "web2", "policy_group": long });
    let response = create_node(&app, body.to_string().as_bytes()).await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "Field 'policy_group' invalid",
    )
    .await;

    let body = json!({ "name": "web3", "policy_name": "example-policy" });
    let response = create_node(&app, body.to_string().as_bytes()).await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn create_rejects_duplicate_node_with_upstream_message() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = create_node(&app, br#"{"name":"web1"}"#).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = create_node(&app, br#"{"name":"web1"}"#).await;
    assert_error(response, StatusCode::CONFLICT, "Node already exists").await;
}

#[tokio::test]
async fn missing_node_reads_use_upstream_not_found_message() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = get_node(&app, "nonexistent_pedant_node").await;
    assert_error(
        response,
        StatusCode::NOT_FOUND,
        "node 'nonexistent_pedant_node' not found",
    )
    .await;

    let response = put_node(
        &app,
        "nonexistent_pedant_node",
        br#"{"json_class":"Chef::Node","run_list":[]}"#,
    )
    .await;
    assert_error(
        response,
        StatusCode::NOT_FOUND,
        "node 'nonexistent_pedant_node' not found",
    )
    .await;

    let response = delete_node(&app, "nonexistent_pedant_node").await;
    assert_error(
        response,
        StatusCode::NOT_FOUND,
        "node 'nonexistent_pedant_node' not found",
    )
    .await;
}

#[tokio::test]
async fn update_rejects_name_mismatch() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;
    assert_eq!(
        create_node(&app, br#"{"name":"web1"}"#).await.status(),
        StatusCode::CREATED
    );

    let response = put_node(&app, "web1", br#"{"name":"Not_the_same_name"}"#).await;
    assert_error(response, StatusCode::BAD_REQUEST, "Node name mismatch.").await;
}

#[tokio::test]
async fn update_without_name_uses_url_name_and_refills_defaults() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;
    assert_eq!(
        create_node(&app, br#"{"name":"web1","normal":{"role":"web"}}"#)
            .await
            .status(),
        StatusCode::CREATED
    );

    let response = put_node(&app, "web1", br#"{"default":{"foo":"bar"}}"#).await;
    assert_eq!(response.status(), StatusCode::OK);
    let updated = support::response_json(response).await;
    assert_eq!(updated["name"], "web1");
    assert_eq!(updated["default"], json!({"foo": "bar"}));
    assert_eq!(updated["normal"], json!({}));

    let fetched = support::response_json(get_node(&app, "web1").await).await;
    assert_eq!(fetched["default"], json!({"foo": "bar"}));
}

#[tokio::test]
async fn posts_to_nodes_with_a_trailing_slash() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = request(
        &app,
        "POST",
        "/organizations/acme/nodes/",
        br#"{"name":"web1"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let fetched = support::response_json(get_node(&app, "web1").await).await;
    assert_eq!(fetched["name"], "web1");
}

#[tokio::test]
async fn rejects_request_bodies_larger_than_the_upstream_limit() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let too_large = vec![b'a'; 2_000_001];
    let response = create_node(&app, &too_large).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let at_limit = vec![b'a'; 2_000_000];
    let response = create_node(&app, &at_limit).await;
    assert_ne!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn lists_nodes_scoped_to_an_environment() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = request(
        &app,
        "POST",
        "/organizations/acme/environments",
        br#"{"name":"production"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    assert_eq!(
        create_node(&app, br#"{"name":"default1"}"#).await.status(),
        StatusCode::CREATED
    );
    assert_eq!(
        create_node(&app, br#"{"name":"prod1","chef_environment":"production"}"#)
            .await
            .status(),
        StatusCode::CREATED
    );

    let response = list_environment_nodes(&app, "_default").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        support::response_json(response).await,
        json!({"default1": "http://0.0.0.0:8000/organizations/acme/nodes/default1"})
    );

    let response = list_environment_nodes(&app, "production").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        support::response_json(response).await,
        json!({"prod1": "http://0.0.0.0:8000/organizations/acme/nodes/prod1"})
    );

    let response = list_environment_nodes(&app, "missing").await;
    assert_error(
        response,
        StatusCode::NOT_FOUND,
        "Cannot load environment 'missing'",
    )
    .await;
}
