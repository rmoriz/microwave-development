mod support;

use axum::http::{header, StatusCode};
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
async fn create_rejects_invalid_environment_bodies() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    for (body, message) in [
        (r#"{"description":"missing name"}"#, "Field 'name' missing"),
        (r#"{"name":"prod:blue"}"#, "Field 'name' invalid"),
        (r#"{"name":"prod blue"}"#, "Field 'name' invalid"),
        (
            r#"{"name":"prod","something_random":"value"}"#,
            "Invalid key something_random in request body",
        ),
        (
            r#"{"name":"prod","json_class":"Chef::Role"}"#,
            "Field 'json_class' invalid",
        ),
        (
            r#"{"name":"prod","chef_type":"role"}"#,
            "Field 'chef_type' invalid",
        ),
        (
            r#"{"name":"prod","default_attributes":"nope"}"#,
            "Field 'default_attributes' is not a hash",
        ),
        (
            r#"{"name":"prod","override_attributes":[]}"#,
            "Field 'override_attributes' is not a hash",
        ),
        (
            r#"{"name":"prod","cookbook_versions":"nope"}"#,
            "Field 'cookbook_versions' is not a hash",
        ),
        (
            r#"{"name":"prod","cookbook_versions":{"apache":true}}"#,
            "Invalid value 'true' for cookbook_versions",
        ),
        (
            r#"{"name":"prod","cookbook_versions":{"apache":">=1.0.0"}}"#,
            "Invalid value '>=1.0.0' for cookbook_versions",
        ),
        (
            r#"{"name":"prod","cookbook_versions":{"the cookbook":">= 1.0.0"}}"#,
            "Invalid key 'the cookbook' for cookbook_versions",
        ),
        (
            r#"["not","an","object"]"#,
            "Incorrect JSON type for request body",
        ),
    ] {
        let response = request(
            &app,
            "POST",
            "/organizations/acme/environments",
            body.as_bytes(),
        )
        .await;
        assert_error(response, StatusCode::BAD_REQUEST, message).await;
    }

    let response = request(&app, "GET", "/organizations/acme/environments/prod", b"").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn put_uses_the_url_name_when_name_is_missing() {
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

    let response = request(
        &app,
        "PUT",
        "/organizations/acme/environments/production",
        br#"{"description":"updated"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let updated = support::response_json(response).await;
    assert_eq!(
        updated,
        json!({
            "name": "production",
            "description": "updated",
            "json_class": "Chef::Environment",
            "chef_type": "environment",
            "cookbook_versions": {},
            "default_attributes": {},
            "override_attributes": {}
        })
    );

    let fetched = support::response_json(
        request(
            &app,
            "GET",
            "/organizations/acme/environments/production",
            b"",
        )
        .await,
    )
    .await;
    assert_eq!(fetched, updated);
}

#[tokio::test]
async fn put_rejects_a_mismatched_name() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = request(
        &app,
        "POST",
        "/organizations/acme/environments",
        br#"{"name":"production","description":"original"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = request(
        &app,
        "PUT",
        "/organizations/acme/environments/production",
        br#"{"name":"staging"}"#,
    )
    .await;
    assert_error(
        response,
        StatusCode::BAD_REQUEST,
        "Environment name mismatch.",
    )
    .await;

    let fetched = support::response_json(
        request(
            &app,
            "GET",
            "/organizations/acme/environments/production",
            b"",
        )
        .await,
    )
    .await;
    assert_eq!(fetched["name"], "production");
    assert_eq!(fetched["description"], "original");
}

#[tokio::test]
async fn delete_returns_the_deleted_environment() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = request(
        &app,
        "POST",
        "/organizations/acme/environments",
        br#"{"name":"production","description":"prod","default_attributes":{"tier":"web"}}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = request(
        &app,
        "DELETE",
        "/organizations/acme/environments/production",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let deleted = support::response_json(response).await;
    assert_eq!(
        deleted,
        json!({
            "name": "production",
            "description": "prod",
            "json_class": "Chef::Environment",
            "chef_type": "environment",
            "cookbook_versions": {},
            "default_attributes": {"tier": "web"},
            "override_attributes": {}
        })
    );

    let response = request(
        &app,
        "GET",
        "/organizations/acme/environments/production",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn default_environment_update_and_delete_return_405_with_allow() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = request(
        &app,
        "PUT",
        "/organizations/acme/environments/_default",
        br#"{"name":"_default"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers().get(header::ALLOW).unwrap(), "GET");
    let body = support::response_json(response).await;
    assert_eq!(
        body,
        json!({ "error": ["The '_default' environment cannot be modified."] })
    );

    let response = request(
        &app,
        "DELETE",
        "/organizations/acme/environments/_default",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers().get(header::ALLOW).unwrap(), "GET");
    let body = support::response_json(response).await;
    assert_eq!(
        body,
        json!({ "error": ["The '_default' environment cannot be modified."] })
    );

    let response = request(
        &app,
        "GET",
        "/organizations/acme/environments/_default",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let environment: Value = support::response_json(response).await;
    assert_eq!(environment["name"], "_default");
}
