mod support;

use axum::http::StatusCode;
use microwave_config::Config;
use microwave_crypto::SignVersion;
use serde_json::Value;
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

async fn create_org(app: &axum::Router, name: &str) {
    let body = format!(r#"{{"name":"{name}","full_name":"{name}"}}"#);
    let (status, response) = request(app, "POST", "/organizations", body.as_bytes()).await;
    assert_eq!(status, StatusCode::CREATED, "{response}");
}

async fn create_node(app: &axum::Router, org: &str, name: &str) {
    let body = format!(r#"{{"name":"{name}"}}"#);
    let (status, response) = request(
        app,
        "POST",
        &format!("/organizations/{org}/nodes"),
        body.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{response}");
}

fn license_config() -> Config {
    let mut config = Config::default();
    config.license.node_license = 2;
    config.license.upgrade_url = "https://example.com/upgrade".into();
    config
}

#[tokio::test]
async fn license_uses_configured_limit_url_and_a_global_node_count() {
    let app = support::app(support::state_with_config(license_config()));
    create_org(&app, "acme").await;
    create_org(&app, "beta").await;

    let (status, body) = request(&app, "GET", "/license", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["node_license"], 2);
    assert_eq!(body["node_count"], 0);
    assert_eq!(body["limit_exceeded"], false);
    assert_eq!(body["upgrade_url"], "https://example.com/upgrade");

    create_node(&app, "acme", "web1").await;
    create_node(&app, "acme", "web2").await;

    let (status, body) = request(&app, "GET", "/license", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["node_count"], 2);
    assert_eq!(body["limit_exceeded"], false);

    create_node(&app, "beta", "db1").await;

    let (status, body) = request(&app, "GET", "/license", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["node_count"], 3);
    assert_eq!(body["limit_exceeded"], true);
}
