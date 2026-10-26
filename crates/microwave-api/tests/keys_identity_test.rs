mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use serde_json::Value;
use tower::ServiceExt;

async fn send(
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
    (status, support::response_json(response).await)
}

#[tokio::test]
async fn a_user_cannot_manage_a_same_named_client_key() {
    let state = support::state_with_pivotal();
    let user = support::add_user(&state, "shared", "shared@example.com", false);
    let org = support::add_org(&state, "acme").await;
    support::associate(&state, &org, &user);
    state
        .sqlite()
        .create_client(&org.id, "shared", false, false)
        .unwrap();

    let app = support::app(state);
    let body = format!(
        r#"{{"name":"alt","public_key":{},"expiration_date":"infinity"}}"#,
        serde_json::to_string(support::PUBLIC_KEY).unwrap()
    );
    let body = body.as_bytes();
    let (status, response) = send(
        &app,
        "POST",
        "/organizations/acme/clients/shared/keys",
        body,
        "shared",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a user with the same name must not inherit client key access: {response}"
    );

    let (status, _) = send(&app, "POST", "/users/shared/keys", body, "shared").await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "users must still manage their own keys"
    );
}
