mod support;

use axum::http::{header::HeaderName, HeaderValue, StatusCode};
use microwave_config::Config;
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
use tower::ServiceExt;

fn tls_config() -> Config {
    let mut config = Config::default();
    config.server.tls.cert_path = Some("/tmp/tls.crt".into());
    config.server.tls.key_path = Some("/tmp/tls.key".into());
    config
}

/// Send a signed request with an optional absolute-form URI (as produced by an
/// HTTP/2 `:authority`) and extra headers.
async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
    authority: Option<&str>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut request = support::signed_request(method, path, body, SignVersion::V1_3, "pivotal");
    if let Some(authority) = authority {
        *request.uri_mut() = format!("https://{authority}{path}").parse().unwrap();
    }
    for (name, value) in headers {
        request.headers_mut().insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    (status, support::response_json(response).await)
}

async fn create_org(app: &axum::Router) {
    let (status, _) = send(
        app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn native_tls_uses_the_request_authority() {
    let state = support::state_with_config(tls_config());
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    // HTTP/2 sends `:authority` and no Host header. The URL must use the
    // authority the client connected to, not the configured bind address.
    let (status, listed) = send(
        &app,
        "GET",
        "/users",
        b"",
        Some("chef.example.com:8443"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed["alice"], "https://chef.example.com:8443/users/alice",
        "HTTP/2 authority must drive generated URLs"
    );
}

#[tokio::test]
async fn x_forwarded_host_wins_over_the_request_authority() {
    let state = support::state_with_config(tls_config());
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let (status, listed) = send(
        &app,
        "GET",
        "/users",
        b"",
        Some("chef.example.com:8443"),
        &[("x-forwarded-host", "proxy.example.com")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed["alice"], "https://proxy.example.com/users/alice",
        "X-Forwarded-Host must take precedence over the authority"
    );
}

#[tokio::test]
async fn forwarded_proto_still_wins_over_native_tls() {
    let state = support::state_with_config(tls_config());
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let (status, listed) = send(
        &app,
        "GET",
        "/users",
        b"",
        Some("chef.example.com:8443"),
        &[("x-forwarded-proto", "http")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["alice"], "http://chef.example.com:8443/users/alice");
}

#[tokio::test]
async fn http1_host_header_still_generates_http_urls() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let (status, listed) = send(
        &app,
        "GET",
        "/users",
        b"",
        None,
        &[("host", "chef.example.com:8000")],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["alice"], "http://chef.example.com:8000/users/alice");
}

#[tokio::test]
async fn local_bookshelf_urls_use_https_under_native_tls() {
    let app = support::app(support::state_with_config(tls_config()));
    create_org(&app).await;

    let checksum = "5d41402abc4b2a76b9719d911017c592";
    let body = serde_json::to_vec(&json!({
        "checksums": serde_json::Map::from_iter([(checksum.to_string(), Value::Null)])
    }))
    .unwrap();
    let (status, created) = send(
        &app,
        "POST",
        "/organizations/acme/sandboxes",
        &body,
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let url = created["checksums"][checksum]["url"].as_str().unwrap();
    assert!(
        url.starts_with("https://0.0.0.0:8000/"),
        "bookshelf URL must follow the TLS listener: {url}"
    );
}
