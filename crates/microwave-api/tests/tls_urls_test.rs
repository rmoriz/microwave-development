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

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
    forwarded_proto: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = support::signed_request(method, path, body, SignVersion::V1_3, "pivotal");
    if let Some(proto) = forwarded_proto {
        request.headers_mut().insert(
            HeaderName::from_static("x-forwarded-proto"),
            HeaderValue::from_str(proto).unwrap(),
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
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn native_tls_generates_https_urls() {
    let state = support::state_with_config(tls_config());
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let (status, listed) = send(&app, "GET", "/users", b"", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed["alice"], "https://0.0.0.0:8000/users/alice",
        "native TLS must not advertise http URLs"
    );
}

#[tokio::test]
async fn forwarded_proto_still_wins_over_native_tls() {
    let state = support::state_with_config(tls_config());
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let (status, listed) = send(&app, "GET", "/users", b"", Some("http")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["alice"], "http://0.0.0.0:8000/users/alice");
}

#[tokio::test]
async fn plain_http_still_generates_http_urls() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let (status, listed) = send(&app, "GET", "/users", b"", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["alice"], "http://0.0.0.0:8000/users/alice");
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
    let (status, created) = send(&app, "POST", "/organizations/acme/sandboxes", &body, None).await;
    assert_eq!(status, StatusCode::CREATED);
    let url = created["checksums"][checksum]["url"].as_str().unwrap();
    assert!(
        url.starts_with("https://0.0.0.0:8000/"),
        "bookshelf URL must follow the TLS listener: {url}"
    );
}
