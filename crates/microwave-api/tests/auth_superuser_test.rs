mod support;

use axum::http::StatusCode;
use microwave_config::Config;
use serde_json::Value;
use tower::ServiceExt;

async fn get(app: &axum::Router, path: &str, user: &str) -> (StatusCode, Value) {
    send(app, "GET", path, b"", user).await
}

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
            microwave_crypto::SignVersion::V1_3,
            user,
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, support::response_json(response).await)
}

#[tokio::test]
async fn configured_superuser_name_controls_privilege() {
    let mut config = Config::default();
    config.auth.superuser_name = "root".into();
    let state = support::state_with_config(config);
    support::add_user(&state, "root", "root@example.com", false);
    let app = support::app(state);

    let (status, _) = get(&app, "/users", "root").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "configured superuser must be privileged"
    );

    // A user literally named "pivotal" must not inherit superuser privilege
    // when the configured superuser is someone else. `state_with_config` seeds
    // that user with `admin = true`, so it is an ordinary server-admin and may
    // list users; organization creation stays superuser-only.
    let (status, body) = get(&app, "/users", "pivotal").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "server-admins may list users: {body}"
    );
    let (status, body) = send(
        &app,
        "POST",
        "/organizations",
        br#"{"name":"not-allowed","full_name":"Not Allowed"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");

    let (status, body) = send(
        &app,
        "POST",
        "/organizations",
        br#"{"name":"allowed","full_name":"Allowed"}"#,
        "root",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the configured superuser may create organizations: {body}"
    );
}
