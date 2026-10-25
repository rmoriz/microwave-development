mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_config::Config;
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

async fn create_user_with_public_key(app: &axum::Router, username: &str) {
    let body = format!(
        r#"{{"username":"{username}","email":"{username}@example.com","display_name":"{username}","password":"secret","public_key":{}}}"#,
        serde_json::to_string(support::PUBLIC_KEY).unwrap()
    );
    let (status, _) = send(app, "POST", "/users", body.as_bytes(), "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn status_reports_keygen_cache_state() {
    let mut config = Config::default();
    config.keygen.size = 2;
    config.keygen.workers = 1;
    config.keygen.warm_up = true;
    let app = support::app(support::state_with_keygen(config));

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/_status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = support::response_json(response).await;
    assert_eq!(body["status"], "pong");
    assert_eq!(body["keygen"]["status"], "ok");
    assert_eq!(body["keygen"]["cached"], 2);
    assert_eq!(body["keygen"]["capacity"], 2);
    assert_eq!(body["keygen"]["workers"], 1);
    assert_eq!(body["keygen"]["worker_healthy"], true);
    assert_eq!(body["keygen"]["worker_failures"], 0);
    assert_eq!(body["keygen"]["generated_on_demand"], 0);
    assert_eq!(body["indexing"]["mode"], "embedded");
}

#[tokio::test]
async fn key_creation_takes_a_keypair_from_the_cache() {
    let mut config = Config::default();
    // Warm synchronously with no background worker so the assertion is
    // deterministic and does not depend on a scheduling race.
    config.keygen.size = 1;
    config.keygen.workers = 0;
    config.keygen.warm_up = true;
    let state = support::state_with_keygen(config);
    let keygen = state.keygen.clone();
    let app = support::app(state);
    assert_eq!(keygen.status().cached, 1);

    create_user_with_public_key(&app, "alice").await;
    let (status, created) = send(
        &app,
        "POST",
        "/users/alice/keys",
        br#"{"name":"generated","create_key":true,"expiration_date":"infinity"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(created["private_key"]
        .as_str()
        .unwrap()
        .starts_with("-----BEGIN PRIVATE KEY-----"));
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/users/alice/keys/generated"
    );

    let status = keygen.status();
    assert_eq!(status.cached, 0, "the cached keypair was consumed");
    assert_eq!(
        status.generated_on_demand, 0,
        "the request must not generate a key inline when the cache is full"
    );
}
