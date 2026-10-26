mod support;

use axum::http::StatusCode;
use microwave_config::Config;
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
use tower::ServiceExt;

async fn request(app: &axum::Router, method: &str, path: &str, body: &[u8]) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            method,
            path,
            body,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    let status = response.status();
    let value = support::response_json(response).await;
    (status, value)
}

async fn create_org(app: &axum::Router) {
    let (status, _) = request(
        app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

async fn solve(app: &axum::Router) -> (StatusCode, Value) {
    request(
        app,
        "POST",
        "/organizations/acme/environments/_default/cookbook_versions",
        br#"{"run_list":[]}"#,
    )
    .await
}

#[test]
fn depsolver_limits_are_configurable_and_validated() {
    let defaults = Config::default();
    assert_eq!(defaults.depsolver.timeout_ms, 20_000);
    assert_eq!(defaults.depsolver.max_concurrent, 4);

    let config = Config::from_toml_str(
        r#"
        [depsolver]
        timeout_ms = 12_500
        max_concurrent = 2
        "#,
    )
    .unwrap();
    assert_eq!(config.depsolver.timeout_ms, 12_500);
    assert_eq!(config.depsolver.max_concurrent, 2);

    assert!(Config::from_toml_str("[depsolver]\ntimeout_ms = 0\n").is_err());
    assert!(Config::from_toml_str("[depsolver]\nmax_concurrent = 0\n").is_err());

    std::env::set_var("MW_DEPSOLVER__TIMEOUT_MS", "17500");
    std::env::set_var("MW_DEPSOLVER__MAX_CONCURRENT", "3");
    let mut from_env = Config::default();
    from_env.apply_env().unwrap();
    std::env::remove_var("MW_DEPSOLVER__TIMEOUT_MS");
    std::env::remove_var("MW_DEPSOLVER__MAX_CONCURRENT");
    assert_eq!(from_env.depsolver.timeout_ms, 17_500);
    assert_eq!(from_env.depsolver.max_concurrent, 3);
}

#[tokio::test]
async fn solve_succeeds_when_a_slot_is_available() {
    let mut config = Config::default();
    config.depsolver.max_concurrent = 1;
    let state = support::state_with_config(config);
    let app = support::app(state);
    create_org(&app).await;

    let (status, body) = solve(&app).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({}));
}

#[tokio::test]
async fn solve_returns_503_when_all_slots_are_busy() {
    let mut config = Config::default();
    config.depsolver.max_concurrent = 1;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    create_org(&app).await;

    // Occupy the only solver slot, as a slow in-flight solve would.
    let permit = state
        .try_acquire_depsolver()
        .expect("the only solver slot should be free");

    let (status, body) = solve(&app).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body, json!({ "error": ["Dependency solver overloaded"] }));

    // Releasing the slot restores normal service; nothing was queued.
    drop(permit);
    let (status, body) = solve(&app).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
