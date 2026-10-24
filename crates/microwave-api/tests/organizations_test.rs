mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use tower::ServiceExt;

#[tokio::test]
async fn creates_an_organization_and_lists_it() {
    let app = support::app(support::state_with_pivotal());
    let body = br#"{"name":"acme","full_name":"Acme Corp"}"#;
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations",
            body,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = support::response_json(response).await;
    assert_eq!(created["uri"], "http://0.0.0.0:8000/organizations/acme");

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed = support::response_json(response).await;
    assert_eq!(listed["acme"], "http://0.0.0.0:8000/organizations/acme");

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let org = support::response_json(response).await;
    assert_eq!(org["name"], "acme");
    assert_eq!(org["full_name"], "Acme Corp");

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
}

async fn request(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
) -> (StatusCode, serde_json::Value) {
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

#[tokio::test]
async fn creates_a_validator_client_with_a_usable_key() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    let (status, created) = request(
        &app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["clientname"], "acme-validator");
    let private_key = created["private_key"].as_str().unwrap();
    assert!(private_key.contains("BEGIN PRIVATE KEY"));
    assert_eq!(created["chef_key"]["name"], "default");

    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let validator = state
        .sqlite()
        .find_client(&org.id, "acme-validator")
        .unwrap()
        .unwrap();
    assert!(validator.validator);
    assert!(state
        .sqlite()
        .find_actor_key(&validator.id, microwave_store::ActorType::Client, "default")
        .unwrap()
        .is_some());
    let clients_group = state
        .sqlite()
        .find_group(&org.id, "clients")
        .unwrap()
        .unwrap();
    let members = state
        .sqlite()
        .list_group_members(&org.id, &clients_group.authz_id)
        .unwrap();
    assert!(members.clients.contains(&"acme-validator".to_string()));

    // The validator key returned by organization creation works immediately
    // and can create a non-validator client.
    let response = app
        .clone()
        .oneshot(support::signed_request_with_key(
            "POST",
            "/organizations/acme/clients",
            br#"{"name":"web1","validator":false}"#,
            SignVersion::V1_3,
            "acme-validator",
            private_key,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn validates_organization_creation_fields() {
    let app = support::app(support::state_with_pivotal());

    let (status, _) = request(
        &app,
        "POST",
        "/organizations",
        br#"{"name":"acme"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "POST",
        "/organizations",
        br#"{"name":"@!## !@#($@","full_name":"Bad"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "POST",
        "/organizations",
        br#"{"full_name":"No Name"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn updates_organization_full_name_and_rejects_renames() {
    let app = support::app(support::state_with_pivotal());
    let (status, _) = request(
        &app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, updated) = request(
        &app,
        "PUT",
        "/organizations/acme",
        br#"{"name":"acme","full_name":"Acme Updated","org_type":"Business"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["full_name"], "Acme Updated");
    assert_eq!(updated["org_type"], "Business");

    let (status, org) = request(&app, "GET", "/organizations/acme", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(org["full_name"], "Acme Updated");

    let (status, body) = request(
        &app,
        "PUT",
        "/organizations/acme",
        br#"{"name":"other","full_name":"Acme Updated"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"][0], "Field 'name' invalid");

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme",
        br#"{"name":"acme"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme",
        br#"{"name":"acme","private_key":"nope"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
