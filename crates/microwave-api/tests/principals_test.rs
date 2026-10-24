mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use serde_json::Value;
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

#[tokio::test]
async fn principals_return_versioned_shapes_and_real_org_membership() {
    let state = support::state_with_pivotal();
    let member = support::add_user(&state, "member", "member@example.com", false);
    let _outside = support::add_user(&state, "outsider", "outsider@example.com", false);
    let app = support::app(state.clone());
    create_org(&app).await;

    let org = state.store.find_org("acme").unwrap().unwrap();
    support::associate(&state, &org, &member);

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"web1","create_key":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/principals/member",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["principals"][0]["name"], "member");
    assert_eq!(body["principals"][0]["type"], "user");
    assert_eq!(body["principals"][0]["org_member"], true);

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/principals/outsider",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["principals"][0]["name"], "outsider");
    assert_eq!(body["principals"][0]["org_member"], false);

    let (status, body) = request_v0(
        &app,
        "GET",
        "/organizations/acme/principals/web1",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "web1");
    assert_eq!(body["type"], "client");
    assert_eq!(body["org_member"], true);
    assert!(body.get("principals").is_none());

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/principals/web1",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["principals"][0]["name"], "web1");
    assert_eq!(body["principals"][0]["type"], "client");
}

#[tokio::test]
async fn principals_use_upstream_not_found_bodies() {
    let state = support::state_with_pivotal();
    let app = support::app(state);
    create_org(&app).await;

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/principals/not_a_number",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "Cannot find principal not_a_number");
    assert_eq!(body["not_found"], "principal");

    let (status, body) = request_v0(
        &app,
        "GET",
        "/organizations/acme/principals/not_a_number",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "Cannot find principal not_a_number");
    assert_eq!(body["not_found"], "principal");

    let (status, body) = request(
        &app,
        "GET",
        "/organizations/bogus-org/principals/web1",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "Cannot find org bogus-org");
    assert_eq!(body["not_found"], "org");
}

#[tokio::test]
async fn principals_are_readable_without_a_valid_signature_or_org_membership() {
    let state = support::state_with_pivotal();
    let _outside = support::add_user(&state, "outsider", "outsider@example.com", false);
    let app = support::app(state);
    create_org(&app).await;

    // Upstream's principal endpoint does not authenticate the requestor.
    let (status, body) = request(
        &app,
        "GET",
        "/organizations/acme/principals/outsider",
        b"",
        "invalid",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["principals"][0]["name"], "outsider");

    // Unassociated users remain forbidden from regular organization resources.
    let (status, _) = request(&app, "GET", "/organizations/acme/clients", b"", "outsider").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
