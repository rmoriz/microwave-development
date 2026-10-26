mod support;

use axum::http::{HeaderMap, StatusCode};
use microwave_crypto::SignVersion;
use serde_json::Value;
use tower::ServiceExt;

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
) -> (StatusCode, HeaderMap, Value) {
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
    let headers = response.headers().clone();
    (status, headers, support::response_json(response).await)
}

fn location(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::LOCATION)
        .and_then(|value| value.to_str().ok())
}

fn assert_created(status: StatusCode, headers: &HeaderMap, body: &Value, expected: &str) {
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(location(headers), Some(expected), "body: {body}");
    if let Some(uri) = body.get("uri") {
        assert_eq!(uri, expected, "body: {body}");
    }
}

#[tokio::test]
async fn creates_a_user_with_location() {
    let state = support::state_with_pivotal();
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "POST",
        "/users",
        br#"{"username":"alice","email":"alice@example.com","display_name":"Alice","password":"secret"}"#,
    )
    .await;

    assert_created(status, &headers, &body, "http://0.0.0.0:8000/users/alice");
}

#[tokio::test]
async fn renames_a_user_with_location() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "PUT",
        "/users/alice",
        br#"{"username":"alice2","email":"alice@example.com","display_name":"Alice"}"#,
    )
    .await;

    assert_created(status, &headers, &body, "http://0.0.0.0:8000/users/alice2");
}

#[tokio::test]
async fn creates_an_organization_with_location() {
    let state = support::state_with_pivotal();
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
    )
    .await;

    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme",
    );
}

#[tokio::test]
async fn creates_and_renames_a_client_with_location() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"robot","create_key":true}"#,
    )
    .await;
    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/clients/robot",
    );

    let (status, headers, body) = send(
        &app,
        "PUT",
        "/organizations/acme/clients/robot",
        br#"{"name":"robot2"}"#,
    )
    .await;
    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/clients/robot2",
    );
}

#[tokio::test]
async fn creates_a_node_with_location() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/nodes",
        br#"{"name":"web1"}"#,
    )
    .await;

    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/nodes/web1",
    );
}

#[tokio::test]
async fn creates_an_environment_and_a_role_with_location() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/environments",
        br#"{"name":"production"}"#,
    )
    .await;
    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/environments/production",
    );

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/roles",
        br#"{"name":"web"}"#,
    )
    .await;
    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/roles/web",
    );
}

#[tokio::test]
async fn creates_data_bags_and_items_with_location() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/data",
        br#"{"name":"secrets"}"#,
    )
    .await;
    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/data/secrets",
    );

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/data/secrets",
        br#"{"id":"db_password","value":"hunter2"}"#,
    )
    .await;
    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/data/secrets/db_password",
    );
}

#[tokio::test]
async fn creates_and_renames_a_group_with_location() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/groups",
        br#"{"groupname":"operators"}"#,
    )
    .await;
    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/groups/operators",
    );

    let (status, headers, body) = send(
        &app,
        "PUT",
        "/organizations/acme/groups/operators",
        br#"{"groupname":"admins2"}"#,
    )
    .await;
    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/groups/admins2",
    );
}

#[tokio::test]
async fn creates_a_container_with_location() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/containers",
        br#"{"containername":"custom"}"#,
    )
    .await;

    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/containers/custom",
    );
}

#[tokio::test]
async fn creates_a_sandbox_with_location() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/sandboxes",
        br#"{"checksums":{"5d41402abc4b2a76b9719d911017c592":null}}"#,
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let expected = format!(
        "http://0.0.0.0:8000/organizations/acme/sandboxes/{}",
        body["sandbox_id"].as_str().unwrap()
    );
    assert_eq!(location(&headers), Some(expected.as_str()));
    assert_eq!(body["uri"], expected);
}

#[tokio::test]
async fn creates_a_cookbook_version_with_location() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "PUT",
        "/organizations/acme/cookbooks/apache/1.0.0",
        br#"{"name":"apache-1.0.0","cookbook_name":"apache","version":"1.0.0","json_class":"Chef::CookbookVersion","chef_type":"cookbook_version","frozen?":false,"metadata":{"version":"1.0.0"},"all_files":[]}"#,
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(
        location(&headers),
        Some("http://0.0.0.0:8000/organizations/acme/cookbooks/apache/1.0.0")
    );
}

#[tokio::test]
async fn creates_a_cookbook_artifact_with_location() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "PUT",
        "/organizations/acme/cookbook_artifacts/apache/abc123",
        br#"{"name":"apache","identifier":"abc123"}"#,
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(
        location(&headers),
        Some("http://0.0.0.0:8000/organizations/acme/cookbook_artifacts/apache/abc123")
    );
}

#[tokio::test]
async fn rejects_environment_and_role_renames_without_location() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    send(
        &app,
        "POST",
        "/organizations/acme/environments",
        br#"{"name":"production"}"#,
    )
    .await;
    let (status, _, _) = send(
        &app,
        "PUT",
        "/organizations/acme/environments/production",
        br#"{"name":"staging"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    send(
        &app,
        "POST",
        "/organizations/acme/roles",
        br#"{"name":"web"}"#,
    )
    .await;
    let (status, _, _) = send(
        &app,
        "PUT",
        "/organizations/acme/roles/web",
        br#"{"name":"web2"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn creates_memberships_and_invitations_with_location() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    support::add_user(&state, "alice", "alice@example.com", false);
    support::add_user(&state, "bob", "bob@example.com", false);
    let app = support::app(state);

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/users",
        br#"{"username":"alice"}"#,
    )
    .await;
    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/users/alice",
    );

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/association_requests",
        br#"{"user":"bob"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let uri = body["uri"].as_str().unwrap();
    assert!(uri.starts_with("http://0.0.0.0:8000/organizations/acme/association_requests/"));
    assert_eq!(location(&headers), Some(uri));
}

#[tokio::test]
async fn creates_policies_revisions_groups_and_associations_with_location() {
    let state = support::state_with_pivotal();
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let revision = br#"{"name":"app","revision_id":"rev1","run_list":["recipe[app::default]"],"cookbook_locks":{"app":{"identifier":"abc123","version":"1.0.0"}}}"#;

    let (status, headers, body) =
        send(&app, "POST", "/organizations/acme/policies", revision).await;
    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/policies/app",
    );

    let revision2 = br#"{"name":"app","revision_id":"rev2","run_list":["recipe[app::default]"],"cookbook_locks":{"app":{"identifier":"def456","version":"1.0.0"}}}"#;
    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/policies/app/revisions",
        revision2,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(
        location(&headers),
        Some("http://0.0.0.0:8000/organizations/acme/policies/app/revisions/rev2")
    );

    let (status, headers, body) = send(
        &app,
        "POST",
        "/organizations/acme/policy_groups",
        br#"{"name":"prod"}"#,
    )
    .await;
    assert_created(
        status,
        &headers,
        &body,
        "http://0.0.0.0:8000/organizations/acme/policy_groups/prod",
    );

    let (status, headers, body) = send(
        &app,
        "PUT",
        "/organizations/acme/policy_groups/prod/policies/app",
        revision,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(
        location(&headers),
        Some("http://0.0.0.0:8000/organizations/acme/policy_groups/prod/policies/app")
    );
    assert_eq!(body["revision_id"], "rev1");
}
