mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use tower::ServiceExt;

#[tokio::test]
async fn creates_lists_and_reads_a_user() {
    let app = support::app(support::state_with_pivotal());
    let body = br#"{"username":"alice","email":"alice@example.com","password":"secret","create_key":true}"#;
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/users",
            body,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = support::response_json(response).await;
    assert_eq!(created["uri"], "http://0.0.0.0:8000/users/alice");
    assert!(created["chef_key"]["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/users",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed = support::response_json(response).await;
    assert_eq!(listed["alice"], "http://0.0.0.0:8000/users/alice");

    let response = app
        .oneshot(support::signed_request(
            "GET",
            "/users/alice",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let user = support::response_json(response).await;
    assert_eq!(user["username"], "alice");
    assert_eq!(user["email"], "alice@example.com");
}

#[tokio::test]
async fn duplicate_user_conflicts() {
    let app = support::app(support::state_with_pivotal());
    let body = br#"{"username":"alice","email":"alice@example.com","password":"secret"}"#;
    for expected in [StatusCode::CREATED, StatusCode::CONFLICT] {
        let response = app
            .clone()
            .oneshot(support::signed_request(
                "POST",
                "/users",
                body,
                SignVersion::V1_3,
                "pivotal",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
}
