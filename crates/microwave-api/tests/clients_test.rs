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

/// Send a request using the legacy API v0 semantics.
async fn request_v0(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request_with_api_version(
            method,
            path,
            body,
            SignVersion::V1_3,
            user,
            "0",
        ))
        .await
        .unwrap();
    let status = response.status();
    let value = support::response_json(response).await;
    (status, value)
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
    org_name: &str,
    group: &str,
    user: &microwave_store::User,
) {
    let org = state.sqlite().find_org(org_name).unwrap().unwrap();
    if !state.sqlite().is_user_in_org(&org.id, &user.id).unwrap() {
        state.sqlite().add_user_to_org(&org.id, &user.id).unwrap();
    }
    let group = state.sqlite().find_group(&org.id, group).unwrap().unwrap();
    state
        .sqlite()
        .add_group_member(&group.authz_id, &user.authz_id)
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
    // API v1+ reports the created key through its location instead of
    // embedding the key material at the top level.
    assert_eq!(
        created,
        json!({
            "uri": "http://0.0.0.0:8000/organizations/acme/clients/public-key-client",
            "chef_key": {
                "uri": "http://0.0.0.0:8000/organizations/acme/clients/public-key-client/keys/default",
                "name": "default",
                "public_key": support::PUBLIC_KEY,
                "expiration_date": "infinity",
            }
        })
    );

    let (status, client) = request(
        &app,
        "GET",
        "/organizations/acme/clients/public-key-client",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(client.get("public_key").is_none());
    assert_eq!(client["clientname"], "public-key-client");
    assert_eq!(client["json_class"], "Chef::ApiClient");
    assert_eq!(client["chef_type"], "client");
    assert_eq!(client["orgname"], "acme");

    // The stored key is still reachable through the keys API.
    let (status, key) = request(
        &app,
        "GET",
        "/organizations/acme/clients/public-key-client/keys/default",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key["public_key"], support::PUBLIC_KEY);
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
async fn client_creation_key_semantics_are_version_aware() {
    let (_state, app, _validator_key) = create_org_with_validator().await;

    // v1+: no create_key and no public_key means no default key at all.
    let (status, created) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"no-key","validator":false}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        created,
        json!({"uri": "http://0.0.0.0:8000/organizations/acme/clients/no-key"})
    );
    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/clients/no-key/keys/default",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Explicit create_key:false behaves the same way.
    let (status, created) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"no-key-explicit","create_key":false}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(created.get("chef_key").is_none());

    // v0 still generates a default key pair and reports both halves at the
    // top level, which is the pre-key-endpoint contract.
    let (status, created) = request_v0(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"legacy-key"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(created["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));
    assert!(created["public_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PUBLIC KEY"));
    let (status, legacy) = request_v0(
        &app,
        "GET",
        "/organizations/acme/clients/legacy-key",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(legacy["public_key"].is_string());
    // The same client is key-free on the v1 representation.
    let (status, modern) = request(
        &app,
        "GET",
        "/organizations/acme/clients/legacy-key",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(modern.get("public_key").is_none());

    // v1+ rejects explicit key generation flags on create.
    for payload in [
        json!({"name":"bad-key-flag","private_key": true}),
        json!({"name":"bad-key-flag","private_key": false}),
        json!({"name":"bad-key-flag","private_key": null}),
    ] {
        let (status, _) = request(
            &app,
            "POST",
            "/organizations/acme/clients",
            payload.to_string().as_bytes(),
            "pivotal",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
    }
    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        json!({"name":"bad-key-flag","public_key": support::PUBLIC_KEY, "create_key": true})
            .to_string()
            .as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = request(
        &app,
        "GET",
        "/organizations/acme/clients/bad-key-flag",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
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
    assert!(updated.get("public_key").is_none());

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/client-b",
        br#"{"name":"client-b"}"#,
        "client-a",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A normal client cannot promote itself to validator.
    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/client-a",
        br#"{"name":"client-a","validator":true}"#,
        "client-a",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Admins may promote and demote clients.
    let (status, updated) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/client-a",
        br#"{"name":"client-a","validator":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["validator"], true);
    let (status, updated) = request(
        &app,
        "PUT",
        "/organizations/acme/clients/client-a",
        br#"{"name":"client-a","validator":false}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["validator"], false);

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

    // API v1+ rejects key management through the client resource; keys are
    // managed through /keys from v1 onward.
    for payload in [
        json!({"name":"client-b","public_key": support::PUBLIC_KEY}),
        json!({"name":"client-b","private_key": true}),
        json!({"name":"client-b","private_key": false}),
        json!({"name":"client-b","private_key": 1}),
        json!({"name":"client-b","create_key": true}),
    ] {
        let (status, _) = request(
            &app,
            "PUT",
            "/organizations/acme/clients/client-b",
            payload.to_string().as_bytes(),
            "pivotal",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
    }
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

    // v0 retains embedded key rotation.
    let replacement = json!({"name":"client-b","public_key": support::PUBLIC_KEY});
    let (status, updated) = request_v0(
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

    let generated = json!({"name":"client-b","private_key": true});
    let (status, updated) = request_v0(
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

#[tokio::test]
async fn newly_created_validator_can_create_clients() {
    let (_state, app, _validator_key) = create_org_with_validator().await;

    let (status, created) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"second-validator","validator":true,"create_key":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let key = created["chef_key"]["private_key"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, created) = request_with_key(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"from-second-validator","create_key":true}"#,
        "second-validator",
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(created["chef_key"]["private_key"].is_string());

    let (status, _) = request_with_key(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"forbidden-third-validator","validator":true}"#,
        "second-validator",
        &key,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
