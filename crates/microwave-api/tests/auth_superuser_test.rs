mod support;

use axum::http::StatusCode;
use microwave_config::Config;
use serde_json::Value;
use tower::ServiceExt;

async fn get(app: &axum::Router, path: &str, user: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            path,
            b"",
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

    // A user literally named "pivotal" must not inherit privilege when the
    // configured superuser is someone else. `state_with_config` seeds that user.
    let (status, body) = get(&app, "/users", "pivotal").await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}
