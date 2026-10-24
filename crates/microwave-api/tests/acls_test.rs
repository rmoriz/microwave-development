mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use microwave_store::ActorType;
use serde_json::Value;
use tower::ServiceExt;

async fn request(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            method,
            path,
            body,
            SignVersion::V1_3,
            user,
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

async fn create_node(app: &axum::Router) {
    let (status, _) = request(
        app,
        "POST",
        "/organizations/acme/nodes",
        br#"{"name":"web1","run_list":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

fn add_user(state: &microwave_api::AppState, name: &str) {
    let user = state
        .store
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
        .store
        .add_actor_key(&user.id, ActorType::User, "default", support::PUBLIC_KEY)
        .unwrap();
}

#[tokio::test]
async fn reads_the_default_organization_acl_and_compatibility_route() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/organizations/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for permission in ["create", "read", "update", "delete", "grant"] {
        assert!(acl[permission]["actors"].is_array());
        assert!(acl[permission]["groups"].is_array());
    }
    assert!(acl["create"]["actors"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("pivotal")));
    assert!(acl["create"]["groups"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("admins")));

    let (status, compatibility) = request(
        &app,
        "GET",
        "/organizations/acme/anything/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(compatibility, acl);

    let (status, granular) = request(
        &app,
        "GET",
        "/organizations/acme/organizations/_acl?detail=granular",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(granular["create"]["actors"].is_null());
    assert!(granular["create"]["users"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("pivotal")));
    assert!(granular["create"]["clients"].as_array().unwrap().is_empty());
    assert!(granular["create"]["groups"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("admins")));
}

#[tokio::test]
async fn organization_acl_put_accepts_a_permission_envelope() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/organizations/_acl/read",
        br#"{"read":{"actors":["pivotal"],"groups":["admins"]}}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/organizations/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(acl["read"]["actors"], serde_json::json!(["pivotal"]));
    assert_eq!(acl["read"]["groups"], serde_json::json!(["admins"]));
}

#[tokio::test]
async fn node_acl_grants_read_without_granting_update() {
    let state = support::state_with_pivotal();
    add_user(&state, "second");
    let app = support::app(state);
    create_org(&app).await;
    create_node(&app).await;

    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/read",
        br#"{"actors":["second"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({}));

    let (status, _) = request(&app, "GET", "/organizations/acme/nodes/web1", b"", "second").await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1",
        br#"{"name":"web1","run_list":["recipe[apache]"]}"#,
        "second",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Modifying the ACL itself requires grant, which second does not have.
    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/update",
        br#"{"actors":["second"],"groups":[]}"#,
        "second",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/nodes/web1/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(acl["read"]["actors"], serde_json::json!(["second"]));
    assert!(acl["update"]["actors"]
        .as_array()
        .unwrap()
        .contains(&serde_json::json!("pivotal")));
}

#[tokio::test]
async fn granular_acl_splits_users_and_clients() {
    let state = support::state_with_pivotal();
    add_user(&state, "second");
    let app = support::app(state.clone());
    create_org(&app).await;
    create_node(&app).await;
    let org = state.store.find_org("acme").unwrap().unwrap();
    state
        .store
        .create_client(&org.id, "robot", false, false)
        .unwrap();

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/read",
        br#"{"users":["second"],"clients":["robot"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, granular) = request(
        &app,
        "GET",
        "/organizations/acme/nodes/web1/_acl?detail=granular",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(granular["read"]["users"], serde_json::json!(["second"]));
    assert_eq!(granular["read"]["clients"], serde_json::json!(["robot"]));
    assert_eq!(granular["read"]["groups"], serde_json::json!([]));
    assert!(granular["read"]["actors"].is_null());

    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/nodes/web1/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let actors = acl["read"]["actors"].as_array().unwrap();
    assert!(actors.contains(&serde_json::json!("second")));
    assert!(actors.contains(&serde_json::json!("robot")));
}

#[tokio::test]
async fn rejects_invalid_acl_payloads_and_missing_targets() {
    let state = support::state_with_pivotal();
    add_user(&state, "second");
    let app = support::app(state);
    create_org(&app).await;
    create_node(&app).await;

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/read",
        br#"{"actors":["nobody"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/read",
        br#"{"actors":["second"],"users":["second"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/read",
        br#"{"actors":[],"groups":["missing"]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/nodes/web1/_acl/not-a-permission",
        br#"{"actors":[],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/nodes/missing/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/groups/missing/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = request(&app, "GET", "/users/missing/_acl", b"", "pivotal").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn user_acls_are_global_and_use_actor_names() {
    let state = support::state_with_pivotal();
    add_user(&state, "second");
    let app = support::app(state.clone());
    create_org(&app).await;
    let org = state.store.find_org("acme").unwrap().unwrap();
    state
        .store
        .create_client(&org.id, "robot", false, false)
        .unwrap();

    let (status, acl) = request(&app, "GET", "/users/second/_acl", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    for permission in ["create", "read", "update", "delete", "grant"] {
        assert_eq!(acl[permission]["actors"], serde_json::json!([]));
        assert_eq!(acl[permission]["groups"], serde_json::json!([]));
    }

    let (status, _) = request(
        &app,
        "PUT",
        "/users/second/_acl/read",
        br#"{"actors":["pivotal"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, acl) = request(&app, "GET", "/users/second/_acl", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(acl["read"]["actors"], serde_json::json!(["pivotal"]));

    let (status, _) = request(
        &app,
        "PUT",
        "/users/second/_acl/read",
        br#"{"actors":["robot"],"groups":[]}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
