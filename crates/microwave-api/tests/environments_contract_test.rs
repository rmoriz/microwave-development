mod support;

use axum::http::{header, HeaderName, HeaderValue, StatusCode};
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

async fn request_with_headers(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> Response {
    let mut request = support::signed_request(method, path, body, SignVersion::V1_3, "pivotal");
    for (name, value) in headers {
        request.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    app.clone().oneshot(request).await.unwrap()
}

async fn assert_error(response: Response, status: StatusCode, message: &str) {
    assert_eq!(response.status(), status);
    let body = support::response_json(response).await;
    assert_eq!(body, json!({ "error": [message] }));
}

async fn put_cookbook(app: &axum::Router, name: &str, version: &str) {
    let manifest = json!({
        "name": format!("{name}-{version}"),
        "cookbook_name": name,
        "version": version,
        "json_class": "Chef::CookbookVersion",
        "chef_type": "cookbook_version",
        "frozen?": false,
        "metadata": { "version": version, "dependencies": {} },
        "all_files": [],
    });
    let response = request(
        app,
        "PUT",
        &format!("/organizations/acme/cookbooks/{name}/{version}"),
        serde_json::to_string(&manifest).unwrap().as_bytes(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn solve(app: &axum::Router, environment: &str, run_list: Value) -> (StatusCode, Value) {
    let body = json!({ "run_list": run_list });
    let response = request(
        app,
        "POST",
        &format!("/organizations/acme/environments/{environment}/cookbook_versions"),
        serde_json::to_string(&body).unwrap().as_bytes(),
    )
    .await;
    let status = response.status();
    (status, support::response_json(response).await)
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
            r#"{"name":"prod","cookbook_versions":{"apache":">= 1.2.9223372036854775849"}}"#,
            "Invalid value '>= 1.2.9223372036854775849' for cookbook_versions",
        ),
        (
            r#"{"name":"prod","cookbook_versions":{"apache":">= 1.0.0.0"}}"#,
            "Invalid value '>= 1.0.0.0' for cookbook_versions",
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
async fn put_without_a_name_is_rejected() {
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
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        support::response_json(response).await,
        json!({"error": ["Field 'name' missing"]})
    );
}

#[tokio::test]
async fn put_renames_an_environment_and_keeps_its_identity() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;

    let response = request(
        &app,
        "POST",
        "/organizations/acme/environments",
        br#"{"name":"production","description":"original","default_attributes":{"tier":"web"}}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let before = state
        .sqlite()
        .find_environment(&org.id, "production")
        .unwrap()
        .unwrap();

    let response = request(
        &app,
        "PUT",
        "/organizations/acme/environments/production",
        br#"{"name":"staging","description":"renamed"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "http://0.0.0.0:8000/organizations/acme/environments/staging"
    );
    let renamed = support::response_json(response).await;
    assert_eq!(
        renamed,
        json!({
            "name": "staging",
            "description": "renamed",
            "json_class": "Chef::Environment",
            "chef_type": "environment",
            "cookbook_versions": {},
            "default_attributes": {},
            "override_attributes": {}
        })
    );

    let after = state
        .sqlite()
        .find_environment(&org.id, "staging")
        .unwrap()
        .unwrap();
    assert_eq!(after.id, before.id);
    assert_eq!(after.authz_id, before.authz_id);
    assert!(state
        .sqlite()
        .find_environment(&org.id, "production")
        .unwrap()
        .is_none());
    let fetched = support::response_json(
        request(&app, "GET", "/organizations/acme/environments/staging", b"").await,
    )
    .await;
    assert_eq!(fetched, renamed);
}

#[tokio::test]
async fn put_requires_an_environment_name() {
    let state = support::state_with_pivotal();
    let app = support::app(state);
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
        br#"{"description":"no name"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        support::response_json(response).await,
        json!({"error": ["Field 'name' missing"]})
    );
}

#[tokio::test]
async fn put_with_a_missing_name_is_rejected_before_any_change() {
    // Covered by put_requires_an_environment_name; kept as a named regression
    // for the upstream Pedant case where the name key is deleted.
    let state = support::state_with_pivotal();
    let app = support::app(state);
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
        br#"{"description":"x"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn create_and_update_negotiate_json_or_return_406_not_acceptable() {
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

    for (method, path, body, headers) in [
        (
            "POST",
            "/organizations/acme/environments",
            br#"{"name":"staging"}"#.as_slice(),
            vec![("accept", "application/xml")],
        ),
        (
            "PUT",
            "/organizations/acme/environments/production",
            br#"{"description":"updated"}"#.as_slice(),
            vec![("accept", "application/xml")],
        ),
        (
            "POST",
            "/organizations/acme/environments",
            br#"{"name":"staging"}"#.as_slice(),
            vec![("content-type", "application/xml")],
        ),
        (
            "PUT",
            "/organizations/acme/environments/production",
            br#"{"description":"updated"}"#.as_slice(),
            vec![("content-type", "application/xml")],
        ),
    ] {
        let response = request_with_headers(&app, method, path, body, &headers).await;
        assert_eq!(
            response.status(),
            StatusCode::NOT_ACCEPTABLE,
            "{method} {path} {headers:?}"
        );
        let body = support::response_json(response).await;
        assert_eq!(body, json!({ "error": ["Not Acceptable"] }));
    }

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
    assert_eq!(fetched["description"], "");

    let response = request(&app, "GET", "/organizations/acme/environments/staging", b"").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
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

#[tokio::test]
async fn stores_cookbook_constraints_so_solves_apply_them() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;
    put_cookbook(&app, "app", "1.0.0").await;
    put_cookbook(&app, "app", "2.0.0").await;

    let response = request(
        &app,
        "POST",
        "/organizations/acme/environments",
        br#"{"name":"pinned","cookbook_versions":{"app":"= 1.0.0"}}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let fetched = support::response_json(
        request(&app, "GET", "/organizations/acme/environments/pinned", b"").await,
    )
    .await;
    assert_eq!(fetched["cookbook_versions"], json!({ "app": "= 1.0.0" }));

    // The write path derived the map into its own column.
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let stored = state
        .sqlite()
        .find_environment(&org.id, "pinned")
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.cookbook_constraints.as_deref(),
        Some(r#"{"app":"= 1.0.0"}"#)
    );

    // A run-list version that violates the stored constraint has no versions.
    let (status, body) = solve(&app, "pinned", json!(["app@2.0.0"])).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        body["error"][0]["cookbooks_with_no_versions"],
        json!(["(app = 2.0.0)"])
    );

    // Without a run-list version the constraint decides the pick.
    let (status, body) = solve(&app, "pinned", json!(["app"])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["app"]["version"], "1.0.0");
}
