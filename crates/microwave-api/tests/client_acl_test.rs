mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use microwave_store::ActorType;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn request(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
) -> (StatusCode, Value) {
    request_with_key(app, method, path, body, user, support::PRIVATE_KEY).await
}

async fn request_with_key(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
    key: &str,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request_with_key(
            method,
            path,
            body,
            SignVersion::V1_3,
            user,
            key,
        ))
        .await
        .unwrap();
    let status = response.status();
    let value = support::response_json(response).await;
    (status, value)
}

async fn create_org(app: &axum::Router) {
    let (status, _) = request(
        app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

async fn create_client(app: &axum::Router, name: &str) {
    let body = json!({"name": name, "public_key": support::PUBLIC_KEY});
    let (status, _) = request(
        app,
        "POST",
        "/organizations/acme/clients",
        body.to_string().as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

fn add_user(state: &microwave_api::AppState, name: &str) -> microwave_store::User {
    let user = state
        .sqlite()
        .create_user(
            name,
            &format!("{name}@example.com"),
            "bcrypt",
            "h",
            "s",
            false,
        )
        .unwrap();
    state
        .sqlite()
        .add_actor_key(&user.id, ActorType::User, "default", support::PUBLIC_KEY)
        .unwrap();
    user
}

fn add_member(
    state: &microwave_api::AppState,
    org: &microwave_store::Org,
    group: &str,
    authz_id: &str,
) {
    let group = state.sqlite().find_group(&org.id, group).unwrap().unwrap();
    state
        .sqlite()
        .add_group_member(&group.authz_id, authz_id)
        .unwrap();
}

#[tokio::test]
async fn client_can_read_the_default_environment() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;
    create_client(&app, "node1").await;

    let (status, environment) = request(
        &app,
        "GET",
        "/organizations/acme/environments/_default",
        b"",
        "node1",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(environment["name"], "_default");
    assert_eq!(environment["json_class"], "Chef::Environment");

    // The environment ACL keeps the creator and admins at full control and
    // adds the clients group through container inheritance.
    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/environments/_default/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(acl["read"]["actors"]
        .as_array()
        .unwrap()
        .contains(&json!("pivotal")));
    assert!(acl["read"]["groups"]
        .as_array()
        .unwrap()
        .contains(&json!("admins")));
    assert!(acl["read"]["groups"]
        .as_array()
        .unwrap()
        .contains(&json!("clients")));
    for permission in ["create", "read", "update", "delete", "grant"] {
        assert!(acl[permission]["groups"]
            .as_array()
            .unwrap()
            .contains(&json!("admins")));
    }

    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let admin = add_user(&state, "admin");
    state
        .sqlite()
        .associate_user_org(&org.id, &admin.id)
        .unwrap();
    add_member(&state, &org, "admins", &admin.authz_id);
    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/environments/_default",
        b"",
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn client_can_read_roles_and_nodes_needed_for_a_converge() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;
    let org = state.sqlite().find_org("acme").unwrap().unwrap();

    // A normal user creates a role; the role object inherits the roles
    // container ACL, which grants the clients group read.
    let user = add_user(&state, "normal");
    support::associate(&state, &org, &user);
    add_member(&state, &org, "users", &user.authz_id);
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/roles",
        br#"{"name":"web","run_list":["recipe[apache]"]}"#,
        "normal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // A node created by an administrator must be readable by clients.
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/nodes",
        br#"{"name":"web1","chef_environment":"_default","run_list":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    create_client(&app, "node1").await;

    let (status, role) = request(&app, "GET", "/organizations/acme/roles/web", b"", "node1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(role["name"], "web");

    let (status, node) = request(&app, "GET", "/organizations/acme/nodes/web1", b"", "node1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(node["name"], "web1");

    let (status, roles) = request(&app, "GET", "/organizations/acme/roles", b"", "node1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(roles["web"].is_string());

    let (status, nodes) = request(&app, "GET", "/organizations/acme/nodes", b"", "node1").await;
    assert_eq!(status, StatusCode::OK);
    assert!(nodes["web1"].is_string());

    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/roles/web/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(acl["read"]["groups"]
        .as_array()
        .unwrap()
        .contains(&json!("clients")));
    assert!(acl["read"]["actors"]
        .as_array()
        .unwrap()
        .contains(&json!("normal")));
}
