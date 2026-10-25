//! Integration coverage for the internal/compatibility routes:
//! object identifiers, signed-request validation, analytics controls, and the
//! environment/role read helpers.

mod support;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::response::Response;
use microwave_api::AppState;
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
use tower::ServiceExt;

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

async fn create_org(app: &axum::Router) {
    let response = request(
        app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn create_node(app: &axum::Router, name: &str) {
    let body = serde_json::to_vec(&json!({
        "name": name,
        "chef_environment": "_default",
        "run_list": [],
    }))
    .unwrap();
    let response = request(app, "POST", "/organizations/acme/nodes", &body).await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn create_role(app: &axum::Router, body: Value) {
    let response = request(
        app,
        "POST",
        "/organizations/acme/roles",
        &serde_json::to_vec(&body).unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn create_environment(app: &axum::Router, name: &str, constraints: Value) {
    let body = serde_json::to_vec(&json!({
        "name": name,
        "cookbook_versions": constraints,
    }))
    .unwrap();
    let response = request(app, "POST", "/organizations/acme/environments", &body).await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn body_json(response: Response) -> Value {
    support::response_json(response).await
}

fn state() -> AppState {
    support::state_with_pivotal()
}

fn seed_cookbook(state: &AppState, name: &str, version: &str, recipes: &[&str]) {
    let pivotal = state.sqlite().find_user("pivotal").unwrap().unwrap();
    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let files: Vec<Value> = recipes
        .iter()
        .map(|recipe| json!({ "name": format!("recipes/{recipe}.rb"), "path": format!("recipes/{recipe}.rb") }))
        .collect();
    let manifest = json!({
        "name": format!("{name}-{version}"),
        "cookbook_name": name,
        "version": version,
        "json_class": "Chef::CookbookVersion",
        "chef_type": "cookbook_version",
        "frozen?": false,
        "all_files": files,
        "metadata": { "name": name, "version": version },
    });
    state
        .sqlite()
        .create_cookbook_version(
            &org.id,
            name,
            version,
            &serde_json::to_string(&manifest).unwrap(),
            &[],
            false,
            &pivotal.authz_id,
        )
        .unwrap();
}

#[tokio::test]
async fn node_identifiers_match_the_upstream_shape() {
    let state = state();
    let app = support::app(state);
    create_org(&app).await;
    create_node(&app, "web1").await;

    let response = request(
        &app,
        "GET",
        "/organizations/acme/nodes/web1/_identifiers",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body.as_object().unwrap().len(), 3);
    for key in ["id", "authz_id", "org_id"] {
        let value = body[key].as_str().unwrap();
        assert_eq!(value.len(), 32, "{key} must be a 32-character hex id");
        assert!(value.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    let response = request(
        &app,
        "GET",
        "/organizations/acme/nodes/missing/_identifiers",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(response).await,
        json!({ "error": ["No such 'nodes': 'missing'."] })
    );

    let response = request(
        &app,
        "GET",
        "/organizations/acme/search/bad_object/_identifiers",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(response).await,
        json!({ "error": ["Unsupported object type: 'search'."] })
    );
}

#[tokio::test]
async fn identifiers_are_get_only() {
    let state = state();
    let app = support::app(state);
    create_org(&app).await;
    create_node(&app, "web1").await;

    let response = request(
        &app,
        "POST",
        "/organizations/acme/nodes/web1/_identifiers",
        br#"{}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        response
            .headers()
            .get(header::ALLOW)
            .and_then(|value| value.to_str().ok()),
        Some("GET")
    );
}

#[tokio::test]
async fn identifiers_require_organization_membership() {
    let state = state();
    let app = support::app(state.clone());
    create_org(&app).await;
    create_node(&app, "web1").await;
    support::add_user(&state, "outsider", "outsider@example.test", false);

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/nodes/web1/_identifiers",
            b"",
            SignVersion::V1_3,
            "outsider",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(response).await,
        json!({ "error": ["'outsider' not associated with organization 'acme'"] })
    );
}

#[tokio::test]
async fn validate_reports_the_signing_requestor_for_every_supported_method() {
    let state = state();
    let app = support::app(state);
    create_org(&app).await;

    let signed_path = "/organizations/acme/nodes";
    let validation_path = "/organizations/acme/validate/organizations/acme/nodes";
    for method in ["GET", "POST", "PUT", "DELETE"] {
        // Sign the body of the original request but send no body, exactly like
        // pedant's `construct_request`/`do_request` split.
        let payload = br#"{"foo":"bar"}"#;
        let response = app
            .clone()
            .oneshot(support::signed_request_with_signed_path_and_body(
                method,
                validation_path,
                signed_path,
                payload,
                b"",
                SignVersion::V1_3,
                "pivotal",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "method {method}");
        assert_eq!(
            body_json(response).await,
            json!({ "requestor_name": "pivotal", "requestor_type": "user" })
        );
    }
}

#[tokio::test]
async fn validate_rejects_a_signature_for_a_different_path() {
    let state = state();
    let app = support::app(state);
    create_org(&app).await;

    let response = app
        .clone()
        .oneshot(support::signed_request_with_signed_path(
            "POST",
            "/organizations/acme/validate/dog",
            "/organizations/acme/nodes",
            br#"{"foo":"bar"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn validate_reports_clients_and_rejects_unknown_actors() {
    let state = state();
    let app = support::app(state);
    create_org(&app).await;

    let client_body = serde_json::to_vec(&json!({
        "name": "web1",
        "public_key": support::PUBLIC_KEY,
    }))
    .unwrap();
    let response = request(&app, "POST", "/organizations/acme/clients", &client_body).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = app
        .clone()
        .oneshot(support::signed_request_with_signed_path_and_body(
            "POST",
            "/organizations/acme/validate/organizations/acme/nodes",
            "/organizations/acme/nodes",
            br#"{"foo":"bar"}"#,
            b"",
            SignVersion::V1_3,
            "web1",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!({ "requestor_name": "web1", "requestor_type": "client" })
    );

    let response = app
        .clone()
        .oneshot(support::signed_request_with_signed_path_and_body(
            "POST",
            "/organizations/acme/validate/organizations/acme/nodes",
            "/organizations/acme/nodes",
            br#"{"foo":"bar"}"#,
            b"",
            SignVersion::V1_3,
            "nobody",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn controls_is_gone_for_every_method_and_credential() {
    let state = state();
    let app = support::app(state);
    create_org(&app).await;

    for method in ["POST", "GET", "PUT", "DELETE"] {
        let response = request(&app, method, "/organizations/acme/controls", br#"{}"#).await;
        assert_eq!(response.status(), StatusCode::GONE, "method {method}");
        assert_eq!(
            body_json(response).await,
            json!({ "error": "Resource gone, analytics is no longer supported by Chef Server" })
        );
    }

    // Upstream halts before authentication, so an unsigned request sees the
    // same response.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/organizations/acme/controls")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::GONE);
}

#[tokio::test]
async fn environment_recipes_use_the_best_matching_version() {
    let state = state();
    let app = support::app(state.clone());
    create_org(&app).await;
    create_environment(&app, "prod", json!({})).await;
    create_environment(&app, "pinned", json!({ "apache": "= 1.0.0" })).await;

    seed_cookbook(&state, "apache", "1.0.0", &["default", "one"]);
    seed_cookbook(&state, "apache", "2.0.0", &["default", "two"]);
    seed_cookbook(&state, "nginx", "1.0.0", &["default"]);
    // Nested recipe paths collapse to the basename, matching upstream.
    seed_cookbook(&state, "deep", "1.0.0", &["nested/deep"]);

    let response = request(
        &app,
        "GET",
        "/organizations/acme/environments/prod/recipes",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!(["apache", "apache::two", "deep::deep", "nginx"])
    );

    let response = request(
        &app,
        "GET",
        "/organizations/acme/environments/pinned/recipes",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!(["apache", "apache::one", "deep::deep", "nginx"])
    );

    let response = request(
        &app,
        "GET",
        "/organizations/acme/environments/_default/recipes",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!(["apache", "apache::two", "deep::deep", "nginx"])
    );

    let response = request(
        &app,
        "GET",
        "/organizations/acme/environments/missing/recipes",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(response).await,
        json!({ "error": ["Cannot load environment missing"] })
    );
}

#[tokio::test]
async fn environment_recipes_are_empty_when_constraints_match_nothing() {
    let state = state();
    let app = support::app(state.clone());
    create_org(&app).await;
    create_environment(&app, "impossible", json!({ "apache": "= 9.9.9" })).await;
    seed_cookbook(&state, "apache", "1.0.0", &["default"]);

    let response = request(
        &app,
        "GET",
        "/organizations/acme/environments/impossible/recipes",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!([]));
}

#[tokio::test]
async fn role_environment_run_lists_match_upstream() {
    let state = state();
    let app = support::app(state);
    create_org(&app).await;
    create_environment(&app, "prod", json!({})).await;
    create_environment(&app, "undeclared", json!({})).await;
    create_role(
        &app,
        json!({
            "name": "web",
            "run_list": ["recipe[base]"],
            "env_run_lists": {
                "prod": ["recipe[prod]"],
                "ghost": [],
            },
        }),
    )
    .await;

    let response = request(
        &app,
        "GET",
        "/organizations/acme/roles/web/environments",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!(["_default", "ghost", "prod"])
    );

    let cases = [
        (
            "/organizations/acme/roles/web/environments/_default",
            json!({ "run_list": ["recipe[base]"] }),
        ),
        (
            "/organizations/acme/roles/web/environments/prod",
            json!({ "run_list": ["recipe[prod]"] }),
        ),
        (
            "/organizations/acme/environments/_default/roles/web",
            json!({ "run_list": ["recipe[base]"] }),
        ),
        (
            "/organizations/acme/environments/prod/roles/web",
            json!({ "run_list": ["recipe[prod]"] }),
        ),
    ];
    for (path, expected) in cases {
        let response = request(&app, "GET", path, b"").await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(body_json(response).await, expected, "{path}");
    }

    // A role with no run list for an existing environment returns null.
    let response = request(
        &app,
        "GET",
        "/organizations/acme/roles/web/environments/undeclared",
        b"",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({ "run_list": null }));
}

#[tokio::test]
async fn role_environment_routes_report_missing_roles_and_environments() {
    let state = state();
    let app = support::app(state);
    create_org(&app).await;
    create_environment(&app, "prod", json!({})).await;
    create_role(
        &app,
        json!({
            "name": "web",
            "run_list": [],
            "env_run_lists": { "prod": ["recipe[prod]"] },
        }),
    )
    .await;

    for path in [
        "/organizations/acme/roles/missing/environments",
        "/organizations/acme/roles/missing/environments/prod",
        "/organizations/acme/environments/prod/roles/missing",
    ] {
        let response = request(&app, "GET", path, b"").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            body_json(response).await,
            json!({ "error": ["Cannot load role missing"] }),
            "{path}"
        );
    }

    for (path, environment) in [
        (
            "/organizations/acme/roles/web/environments/missing",
            "missing",
        ),
        ("/organizations/acme/roles/web/environments/ghost", "ghost"),
        (
            "/organizations/acme/environments/missing/roles/web",
            "missing",
        ),
    ] {
        let response = request(&app, "GET", path, b"").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            body_json(response).await,
            json!({ "error": [format!("Cannot load environment {environment}")] }),
            "{path}"
        );
    }
}

#[tokio::test]
async fn role_environments_is_get_only() {
    let state = state();
    let app = support::app(state);
    create_org(&app).await;
    create_role(&app, json!({ "name": "web", "run_list": [] })).await;

    let response = request(
        &app,
        "PUT",
        "/organizations/acme/roles/web/environments",
        br#"{}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(
        response
            .headers()
            .get(header::ALLOW)
            .and_then(|value| value.to_str().ok()),
        Some("GET")
    );
}
