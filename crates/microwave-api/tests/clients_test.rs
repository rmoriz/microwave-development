mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
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

#[tokio::test]
async fn creates_a_client_and_reads_principals() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/clients",
            br#"{"name":"web1","validator":false,"create_key":true}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = support::response_json(response).await;
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/organizations/acme/clients/web1"
    );
    assert!(created["chef_key"]["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/clients",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed = support::response_json(response).await;
    assert_eq!(
        listed["web1"],
        "http://0.0.0.0:8000/organizations/acme/clients/web1"
    );

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/clients/web1",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let client = support::response_json(response).await;
    assert_eq!(client["name"], "web1");
    assert_eq!(client["validator"], false);

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/principals/web1",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let principals = support::response_json(response).await;
    assert_eq!(principals["principals"][0]["name"], "web1");
    assert_eq!(principals["principals"][0]["type"], "client");

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "DELETE",
            "/organizations/acme/clients/web1",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

use microwave_store::ActorType;
use serde_json::{json, Value};

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

fn add_user(state: &microwave_api::AppState, name: &str) -> String {
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
    user.authz_id
}

fn add_member(state: &microwave_api::AppState, org_name: &str, group: &str, authz_id: &str) {
    let org = state.store.find_org(org_name).unwrap().unwrap();
    let group = state.store.find_group(&org.id, group).unwrap().unwrap();
    state
        .store
        .add_group_member(&group.authz_id, authz_id)
        .unwrap();
}

async fn create_org_with_validator() -> (microwave_api::AppState, axum::Router, String) {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;
    let (status, org) = request(&app, "GET", "/organizations/acme", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    let _ = org;
    // Recreate from the POST response to get the validator key.
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "DELETE",
            "/organizations/acme",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
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
    let created = support::response_json(response).await;
    let private_key = created["private_key"].as_str().unwrap().to_string();
    (state, app, private_key)
}

#[tokio::test]
async fn client_creation_authorization_matches_validator_rules() {
    let (state, app, validator_key) = create_org_with_validator().await;
    let admin = add_user(&state, "admin");
    let normal = add_user(&state, "normal");
    add_member(&state, "acme", "admins", &admin);
    add_member(&state, "acme", "users", &normal);

    let normal_client = json!({"name":"normal-client","public_key": support::PUBLIC_KEY});
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        normal_client.to_string().as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"forbidden-client"}"#,
        "normal-client",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, created) = request_with_key(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"by-validator","create_key":true}"#,
        "acme-validator",
        &validator_key,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(created["chef_key"]["private_key"].is_string());

    let (status, _) = request_with_key(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"forbidden-validator","validator":true}"#,
        "acme-validator",
        &validator_key,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The validator is replaced by the new client in the new ACL.
    let (status, acl) = request(
        &app,
        "GET",
        "/organizations/acme/clients/by-validator/_acl",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let create_actors = acl["create"]["actors"].as_array().unwrap();
    assert!(create_actors.contains(&json!("pivotal")));
    assert!(create_actors.contains(&json!("by-validator")));
    assert!(!create_actors.contains(&json!("acme-validator")));
    assert_eq!(acl["create"]["groups"], json!(["admins"]));
    assert_eq!(acl["read"]["groups"], json!(["admins", "users"]));

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"bad client"}"#,
        "admin",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn client_public_keys_are_validated_and_returned() {
    let (_state, app, _validator_key) = create_org_with_validator().await;

    for public_key in [
        json!(1),
        json!([]),
        json!({}),
        json!(""),
        json!("-----BEGIN PUBLIC KEY-----You have been trolled :-)-----END PUBLIC KEY-----"),
    ] {
        let body = json!({"name":"bad-key-client","public_key":public_key});
        let (status, _) = request(
            &app,
            "POST",
            "/organizations/acme/clients",
            body.to_string().as_bytes(),
            "pivotal",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/clients/bad-key-client",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let body = json!({"name":"public-key-client","public_key": support::PUBLIC_KEY});
    let (status, created) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        body.to_string().as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["public_key"], support::PUBLIC_KEY);
    assert!(created.get("private_key").is_none());
    assert!(created.get("chef_key").is_none());

    let (status, client) = request(
        &app,
        "GET",
        "/organizations/acme/clients/public-key-client",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(client["public_key"], support::PUBLIC_KEY);
    assert_eq!(client["clientname"], "public-key-client");
    assert_eq!(client["json_class"], "Chef::ApiClient");
    assert_eq!(client["chef_type"], "client");
    assert_eq!(client["orgname"], "acme");
}

#[tokio::test]
async fn generated_client_keys_use_the_versioned_response_shape() {
    let (_state, app, _validator_key) = create_org_with_validator().await;
    let (status, created) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"generated-key","create_key":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/organizations/acme/clients/generated-key"
    );
    assert!(created["chef_key"]["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));
    assert!(created.get("private_key").is_none());
}

#[tokio::test]
async fn client_update_key_rotation_and_self_service() {
    let (state, app, validator_key) = create_org_with_validator().await;
    let admin = add_user(&state, "admin");
    add_member(&state, "acme", "admins", &admin);

    let body = json!({"name":"client-a","public_key": support::PUBLIC_KEY});
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        body.to_string().as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let body = json!({"name":"client-b","public_key": support::PUBLIC_KEY});
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        body.to_string().as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // A normal client can update itself but not a different client.
    let (status, updated) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/client-a",
        br#"{"name":"client-a","validator":false}"#,
        "client-a",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["private_key"], false);
    assert_eq!(updated["public_key"], support::PUBLIC_KEY);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/client-b",
        br#"{"name":"client-b"}"#,
        "client-a",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Validators may never update even themselves.
    let (status, _) = request_with_key(
        &app,
        "PUT",
        "/organizations/acme/clients/acme-validator",
        br#"{"name":"acme-validator"}"#,
        "acme-validator",
        &validator_key,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let replacement = json!({"name":"client-b","public_key": support::PUBLIC_KEY});
    let (status, updated) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/client-b",
        replacement.to_string().as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["public_key"], support::PUBLIC_KEY);
    assert_eq!(updated["private_key"], false);

    let bad = json!({"name":"client-b","public_key": 1});
    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/client-b",
        bad.to_string().as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let generated = json!({"name":"client-b","private_key": true});
    let (status, updated) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/client-b",
        generated.to_string().as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(updated["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));

    let (status, renamed) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/client-b",
        br#"{"name":"client-renamed"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(renamed["name"], "client-renamed");
    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/clients/client-b",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Normal clients can delete themselves; validators cannot.
    let (status, _) = request(
        &app,
        "DELETE",
        "/organizations/acme/clients/client-a",
        b"",
        "client-a",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request_with_key(
        &app,
        "DELETE",
        "/organizations/acme/clients/acme-validator",
        b"",
        "acme-validator",
        &validator_key,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let _ = validator_key;
}
