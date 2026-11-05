mod support;

use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::Router;
use microwave_api::AppState;
use microwave_config::Config;
use microwave_crypto::SignVersion;
use serde_json::json;
use std::net::SocketAddr;
use tower::ServiceExt;

fn config() -> Config {
    let mut config = Config::default();
    config.limits.queue_timeout_ms = 50;
    config.limits.source.rate_per_second = 1_000;
    config.limits.source.burst = 1_000;
    config
}

fn peer(request: &mut Request<Body>, address: &str) {
    let peer: SocketAddr = SocketAddr::new(address.parse().unwrap(), 0);
    request.extensions_mut().insert(ConnectInfo(peer));
}

async fn create_org(state: &AppState, name: &str) {
    support::add_org(state, name).await;
}

fn node_request(_state: &AppState, org: &str, address: Option<&str>) -> Request<Body> {
    let mut request = support::signed_request(
        "GET",
        &format!("/organizations/{org}/nodes"),
        b"",
        SignVersion::V1_3,
        "pivotal",
    );
    if let Some(address) = address {
        peer(&mut request, address);
    }
    request
}

async fn node_get(
    app: &Router,
    state: &AppState,
    org: &str,
    address: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(node_request(state, org, Some(address)))
        .await
        .unwrap()
}

async fn node_get_without_peer(
    app: &Router,
    state: &AppState,
    org: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(node_request(state, org, None))
        .await
        .unwrap()
}

#[tokio::test]
async fn saturated_global_limit_answers_503_and_keeps_health_reachable() {
    let mut config = config();
    config.limits.global.in_flight = 1;
    config.limits.queue_timeout_ms = 0;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    create_org(&state, "acme").await;

    let _held = state.limits.global().await.unwrap();
    let response = node_get(&app, &state, "acme", "127.0.0.1").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["retry-after"], "1");
    let body = support::response_json(response).await;
    assert_eq!(
        body,
        json!({ "error": ["Server is overloaded. Retry later."] })
    );

    let status = app
        .clone()
        .oneshot(Request::get("/_status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
}

#[tokio::test]
async fn requests_without_a_peer_skip_only_the_source_bucket() {
    let mut config = config();
    config.limits.source.burst = 1;
    config.limits.source.rate_per_second = 1;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    create_org(&state, "acme").await;

    assert_eq!(
        node_get_without_peer(&app, &state, "acme").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        node_get_without_peer(&app, &state, "acme").await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn disabled_limits_pass_a_saturated_global_slot() {
    let mut config = config();
    config.limits.enabled = false;
    config.limits.global.in_flight = 1;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    create_org(&state, "acme").await;
    let _held = state.limits.global().await.unwrap();
    assert_eq!(
        node_get(&app, &state, "acme", "127.0.0.1").await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn source_bucket_rejects_one_address_without_touching_another() {
    let mut config = config();
    config.limits.source.burst = 1;
    config.limits.source.rate_per_second = 1;
    config.limits.queue_timeout_ms = 0;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    create_org(&state, "acme").await;

    assert_eq!(
        node_get(&app, &state, "acme", "203.0.113.1").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        node_get(&app, &state, "acme", "203.0.113.1").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        node_get(&app, &state, "acme", "203.0.113.2").await.status(),
        StatusCode::OK
    );
}
