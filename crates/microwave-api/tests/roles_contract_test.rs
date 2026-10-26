mod support;

use axum::http::StatusCode;
use axum::response::Response;
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
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

async fn assert_error(response: Response, status: StatusCode, message: &str) {
    assert_eq!(response.status(), status);
    let body = support::response_json(response).await;
    assert_eq!(body, json!({ "error": [message] }));
}

#[tokio::test]
async fn create_normalizes_run_lists_and_sets_defaults() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = request(
        &app,
        "POST",
        "/organizations/acme/roles",
        br#"{
            "name": "web",
            "run_list": ["base", "recipe[base]", "role[web]", "role[web]"],
            "env_run_lists": {
                "prod": ["foo", "recipe[foo]", "role[prod]", "role[prod]"],
                "dev": ["bar::baz@1.0.0", "recipe[bew]"]
            }
        }"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let expected = json!({
        "name": "web",
        "json_class": "Chef::Role",
        "chef_type": "role",
        "default_attributes": {},
        "override_attributes": {},
        "run_list": ["recipe[base]", "role[web]"],
        "env_run_lists": {
            "dev": ["recipe[bar::baz@1.0.0]", "recipe[bew]"],
            "prod": ["recipe[foo]", "role[prod]"]
        }
    });

    let fetched =
        support::response_json(request(&app, "GET", "/organizations/acme/roles/web", b"").await)
            .await;
    assert_eq!(fetched, expected);
}

#[tokio::test]
async fn put_uses_the_url_name_when_name_is_missing() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = request(
        &app,
        "POST",
        "/organizations/acme/roles",
        br#"{"name":"web","run_list":["base"]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = request(
        &app,
        "PUT",
        "/organizations/acme/roles/web",
        br#"{"description":"updated","run_list":["apache"],"env_run_lists":{"prod":["base"]}}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let updated = support::response_json(response).await;
    assert_eq!(updated["name"], "web");
    assert_eq!(updated["description"], "updated");
    assert_eq!(updated["run_list"], json!(["recipe[apache]"]));
    assert_eq!(updated["env_run_lists"], json!({"prod": ["recipe[base]"]}));

    let fetched =
        support::response_json(request(&app, "GET", "/organizations/acme/roles/web", b"").await)
            .await;
    assert_eq!(fetched, updated);
}

#[tokio::test]
async fn put_rejects_a_mismatched_name_and_still_validates_the_body() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = request(
        &app,
        "POST",
        "/organizations/acme/roles",
        br#"{"name":"web","run_list":["base"]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = request(
        &app,
        "PUT",
        "/organizations/acme/roles/web",
        br#"{"name":"other"}"#,
    )
    .await;
    assert_error(response, StatusCode::BAD_REQUEST, "Role name mismatch.").await;

    let response = request(
        &app,
        "PUT",
        "/organizations/acme/roles/web",
        br#"{"json_class":"Chef::Node"}"#,
    )
    .await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "Field 'json_class' invalid",
    )
    .await;

    let fetched =
        support::response_json(request(&app, "GET", "/organizations/acme/roles/web", b"").await)
            .await;
    assert_eq!(fetched["name"], "web");
    assert_eq!(fetched["run_list"], json!(["recipe[base]"]));
}

#[tokio::test]
async fn delete_returns_the_normalized_role_document() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = request(
        &app,
        "POST",
        "/organizations/acme/roles",
        br#"{"name":"web","run_list":["base","role[web]"],"env_run_lists":{"prod":["foo"]}}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = request(&app, "DELETE", "/organizations/acme/roles/web", b"").await;
    assert_eq!(response.status(), StatusCode::OK);
    let deleted = support::response_json(response).await;
    assert_eq!(
        deleted,
        json!({
            "name": "web",
            "json_class": "Chef::Role",
            "chef_type": "role",
            "default_attributes": {},
            "override_attributes": {},
            "run_list": ["recipe[base]", "role[web]"],
            "env_run_lists": {"prod": ["recipe[foo]"]}
        })
    );

    let response = request(&app, "GET", "/organizations/acme/roles/web", b"").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_rejects_invalid_role_bodies() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    for (body, message) in [
        (r#"{"run_list":[]}"#, "Field 'name' missing"),
        (r#"{"name":"bad!name"}"#, "Field 'name' invalid"),
        (
            r#"{"name":"web","something_random":"value"}"#,
            "Invalid key something_random in request body",
        ),
        (
            r#"{"name":"web","json_class":"Chef::Environment"}"#,
            "Field 'json_class' invalid",
        ),
        (
            r#"{"name":"web","chef_type":"environment"}"#,
            "Field 'chef_type' invalid",
        ),
        (
            r#"{"name":"web","default_attributes":"nope"}"#,
            "Field 'default_attributes' is not a hash",
        ),
        (
            r#"{"name":"web","override_attributes":[]}"#,
            "Field 'override_attributes' is not a hash",
        ),
        (
            r#"{"name":"web","run_list":[123]}"#,
            "Field 'run_list' is not a valid run list",
        ),
        (
            r#"{"name":"web","run_list":["recipe["]}"#,
            "Field 'run_list' is not a valid run list",
        ),
        (
            r#"{"name":"web","env_run_lists":"nope"}"#,
            "Field 'env_run_lists' contains invalid run lists",
        ),
        (
            r#"{"name":"web","env_run_lists":{"prod":[123]}}"#,
            "Field 'env_run_lists' contains invalid run lists",
        ),
        (
            r#"{"name":"web","env_run_lists":{"bad env":[]}}"#,
            "Invalid key 'bad env' for env_run_lists",
        ),
        (
            r#"["not","an","object"]"#,
            "Incorrect JSON type for request body",
        ),
    ] {
        let response = request(&app, "POST", "/organizations/acme/roles", body.as_bytes()).await;
        assert_error(response, StatusCode::BAD_REQUEST, message).await;
    }

    let response = request(&app, "GET", "/organizations/acme/roles/web", b"").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn role_names_allow_colons() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = request(
        &app,
        "POST",
        "/organizations/acme/roles",
        br#"{"name":"team:web","run_list":["base"]}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let fetched: Value = support::response_json(
        request(&app, "GET", "/organizations/acme/roles/team:web", b"").await,
    )
    .await;
    assert_eq!(fetched["name"], "team:web");
    assert_eq!(fetched["run_list"], json!(["recipe[base]"]));
}

#[tokio::test]
async fn reads_normalize_legacy_run_lists() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;

    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let pivotal = state.sqlite().find_user("pivotal").unwrap().unwrap();
    state
        .sqlite()
        .create_role(
            &org.id,
            "legacy",
            r#"{"name":"legacy","run_list":["base","role[web]"],"env_run_lists":{"prod":["foo"]}}"#,
            &pivotal.authz_id,
        )
        .unwrap();

    let fetched: Value =
        support::response_json(request(&app, "GET", "/organizations/acme/roles/legacy", b"").await)
            .await;
    assert_eq!(fetched["run_list"], json!(["recipe[base]", "role[web]"]));
    assert_eq!(fetched["env_run_lists"], json!({"prod": ["recipe[foo]"]}));
}
