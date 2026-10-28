mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_api::AppState;
use microwave_config::{Config, McpOrgConfig};
use serde_json::{json, Value};
use tower::ServiceExt;

fn mcp_config(orgs: &[(&str, &str)]) -> Config {
    let mut config = Config::default();
    config.mcp.enabled = true;
    for (org, token) in orgs {
        config.mcp.organizations.insert(
            (*org).to_string(),
            McpOrgConfig {
                token: Some((*token).to_string()),
                ..Default::default()
            },
        );
    }
    config
}

async fn mcp(app: &axum::Router, org: &str, token: &str, body: Value) -> axum::response::Response {
    mcp_with(app, org, Some(token), "localhost", body).await
}

async fn mcp_with(
    app: &axum::Router,
    org: &str,
    token: Option<&str>,
    host: &str,
    body: Value,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/organizations/{org}/mcp"))
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("host", host);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    app.clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "1.0" }
        }
    })
}

fn tools_list(id: u64) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": "tools/list", "params": {} })
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn acme_app(config: Config) -> axum::Router {
    let state: AppState = support::state_with_config(config);
    support::add_org(&state, "acme").await;
    support::app(state)
}

#[tokio::test]
async fn mcp_endpoint_requires_a_token() {
    let app = acme_app(mcp_config(&[("acme", "a")])).await;

    let response = mcp_with(&app, "acme", None, "localhost", initialize()).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );
}

#[tokio::test]
async fn mcp_token_is_bound_to_its_org() {
    let state = support::state_with_config(mcp_config(&[("acme", "a"), ("beta", "b")]));
    support::add_org(&state, "acme").await;
    support::add_org(&state, "beta").await;
    let app = support::app(state);

    let response = mcp(&app, "beta", "a", initialize()).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unknown_or_unconfigured_org_is_404() {
    let state = support::state_with_config(mcp_config(&[("acme", "a")]));
    support::add_org(&state, "acme").await;
    support::add_org(&state, "beta").await;
    let app = support::app(state);

    let missing = mcp(&app, "missing", "a", initialize()).await;
    let unconfigured = mcp(&app, "beta", "a", initialize()).await;

    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(unconfigured.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn disabled_endpoint_is_404() {
    let state = support::state_with_config(Config::default());
    support::add_org(&state, "acme").await;
    let app = support::app(state);

    let response = mcp(&app, "acme", "a", initialize()).await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn initialize_and_tools_list_work() {
    let app = acme_app(mcp_config(&[("acme", "a")])).await;

    let initialized = mcp(&app, "acme", "a", initialize()).await;
    assert_eq!(initialized.status(), StatusCode::OK);
    let initialized = body_json(initialized).await;
    assert!(initialized["result"]["serverInfo"].is_object());

    let listed = mcp(&app, "acme", "a", tools_list(2)).await;
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = body_json(listed).await;
    assert_eq!(listed["result"]["tools"], json!([]));
}

#[tokio::test]
async fn non_loopback_host_is_rejected_unless_configured() {
    let app = acme_app(mcp_config(&[("acme", "a")])).await;
    let rejected = mcp_with(&app, "acme", Some("a"), "evil.example.com", initialize()).await;
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

    let mut config = mcp_config(&[("acme", "a")]);
    config.mcp.allowed_hosts = vec!["evil.example.com".into()];
    let app = acme_app(config).await;
    let allowed = mcp_with(&app, "acme", Some("a"), "evil.example.com", initialize()).await;
    assert_eq!(allowed.status(), StatusCode::OK);
}

#[tokio::test]
async fn oversized_request_body_is_413() {
    let mut config = mcp_config(&[("acme", "a")]);
    config.mcp.max_upload_bytes = 1;
    let app = acme_app(config).await;
    let body = json!({ "padding": "x".repeat(2 * 1024 * 1024) });

    let response = mcp(&app, "acme", "a", body).await;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
