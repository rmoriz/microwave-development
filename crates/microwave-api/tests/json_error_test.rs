mod support;

use axum::http::{header, HeaderValue, StatusCode};
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn send_raw(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            method,
            path,
            body,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, support::response_json(response).await)
}

fn assert_invalid_json(status: StatusCode, body: &Value, context: &str) {
    assert_eq!(status, StatusCode::BAD_REQUEST, "{context}: {body}");
    assert_eq!(
        body,
        &json!({ "error": ["invalid JSON"] }),
        "{context}: expected a Chef-shaped error array, got {body}"
    );
    assert!(
        body["error"].is_array(),
        "{context}: malformed JSON must not produce a plain-text error"
    );
}

#[tokio::test]
async fn malformed_json_is_reported_with_the_chef_error_envelope() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let cases: [(&str, &str); 22] = [
        ("POST", "/users"),
        ("PUT", "/users/pivotal"),
        ("POST", "/organizations"),
        ("PUT", "/organizations/acme"),
        ("POST", "/organizations/acme/clients"),
        ("PUT", "/organizations/acme/clients/robot"),
        ("POST", "/organizations/acme/nodes"),
        ("PUT", "/organizations/acme/nodes/web1"),
        ("POST", "/organizations/acme/environments"),
        ("PUT", "/organizations/acme/environments/production"),
        ("POST", "/organizations/acme/roles"),
        ("PUT", "/organizations/acme/roles/web"),
        ("POST", "/organizations/acme/data"),
        ("POST", "/organizations/acme/data/secrets"),
        ("PUT", "/organizations/acme/data/secrets/item"),
        ("POST", "/organizations/acme/groups"),
        ("PUT", "/organizations/acme/groups/operators"),
        ("POST", "/organizations/acme/containers"),
        ("POST", "/organizations/acme/policies"),
        ("POST", "/organizations/acme/policies/app/revisions"),
        ("POST", "/organizations/acme/policy_groups"),
        ("PUT", "/organizations/acme/policy_groups/prod/policies/app"),
    ];

    for (method, path) in cases {
        let (status, body) = send_raw(&app, method, path, br#"{"name": "#).await;
        assert_invalid_json(status, &body, &format!("{method} {path}"));
    }

    for (method, path) in [
        ("POST", "/organizations/acme/users"),
        ("POST", "/organizations/acme/association_requests"),
        ("PUT", "/users/alice/association_requests/id"),
    ] {
        let (status, body) = send_raw(&app, method, path, br#"{"user": "#).await;
        assert_invalid_json(status, &body, &format!("{method} {path}"));
    }
}

#[tokio::test]
async fn empty_request_bodies_are_invalid_json() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let (status, body) = send_raw(&app, "POST", "/organizations/acme/nodes", b"").await;
    assert_invalid_json(status, &body, "empty body");
}

#[tokio::test]
async fn non_json_content_types_use_the_chef_error_envelope() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let mut request = support::signed_request(
        "POST",
        "/organizations/acme/nodes",
        br#"{"name":"web1"}"#,
        SignVersion::V1_3,
        "pivotal",
    );
    request
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));

    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = support::response_json(response).await;

    assert_invalid_json(status, &body, "text/plain");
    assert!(
        content_type.starts_with("application/json"),
        "error responses must be JSON, got {content_type}"
    );
}

#[tokio::test]
async fn valid_json_still_reaches_the_handler() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let (status, body) = send_raw(
        &app,
        "POST",
        "/organizations/acme/nodes",
        br#"{"name":"web1"}"#,
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(
        body["uri"],
        "http://0.0.0.0:8000/organizations/acme/nodes/web1"
    );
}
