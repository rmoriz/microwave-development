//! HTTP edge hardening: request ids, access logs, compression negotiation, and
//! trusted forwarded headers.

mod support;

use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{header, HeaderName, HeaderValue, Request, StatusCode};
use axum::response::Response;
use axum::Router;
use flate2::read::{GzDecoder, ZlibDecoder};
use microwave_api::AppState;
use microwave_config::Config;
use microwave_crypto::SignVersion;
use serde_json::Value;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

fn state_with_trusted_proxies(trusted: &[&str]) -> AppState {
    let mut config = Config::default();
    config.server.trusted_proxies = trusted.iter().map(ToString::to_string).collect();
    let state = support::state_with_config(config);
    support::add_user(&state, "alice", "alice@example.com", false);
    state
}

fn with_header(request: &mut Request<Body>, name: &str, value: &str) {
    request.headers_mut().insert(
        HeaderName::from_bytes(name.as_bytes()).unwrap(),
        HeaderValue::from_str(value).unwrap(),
    );
}

fn with_peer(request: &mut Request<Body>, peer: SocketAddr) {
    request.extensions_mut().insert(ConnectInfo(peer));
}

async fn get(app: &Router, path: &str, headers: &[(&str, &str)]) -> Response {
    let mut request = Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .unwrap();
    for (name, value) in headers {
        with_header(&mut request, name, value);
    }
    app.clone().oneshot(request).await.unwrap()
}

/// Signed `GET /users` URL for `alice`, using the supplied forwarding headers.
async fn listed_users_url(
    app: &Router,
    host: Option<&str>,
    forwarded_host: Option<&str>,
    forwarded_proto: Option<&str>,
    peer: Option<SocketAddr>,
) -> String {
    let mut request = support::signed_request("GET", "/users", b"", SignVersion::V1_3, "pivotal");
    if let Some(host) = host {
        with_header(&mut request, "host", host);
    }
    if let Some(forwarded_host) = forwarded_host {
        with_header(&mut request, "x-forwarded-host", forwarded_host);
    }
    if let Some(forwarded_proto) = forwarded_proto {
        with_header(&mut request, "x-forwarded-proto", forwarded_proto);
    }
    if let Some(peer) = peer {
        with_peer(&mut request, peer);
    }
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = support::response_json(response).await;
    body["alice"].as_str().unwrap().to_string()
}

// ---------------------------------------------------------------------------
// Request ids and access log
// ---------------------------------------------------------------------------

#[tokio::test]
async fn generates_a_request_id_when_the_client_sends_none() {
    let app = support::app(support::state_with_pivotal());
    let response = get(&app, "/version", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let request_id = response
        .headers()
        .get("x-request-id")
        .expect("generated request id")
        .to_str()
        .unwrap();
    assert!(
        Uuid::parse_str(request_id).is_ok(),
        "generated request id should be a UUID, got {request_id:?}"
    );
}

#[tokio::test]
async fn preserves_a_client_supplied_request_id() {
    let app = support::app(support::state_with_pivotal());
    let response = get(&app, "/version", &[("x-request-id", "client-request-123")]).await;
    assert_eq!(
        response.headers().get("x-request-id").unwrap(),
        "client-request-123"
    );
}

#[tokio::test]
async fn replaces_a_malformed_client_request_id() {
    let app = support::app(support::state_with_pivotal());
    let response = get(&app, "/version", &[("x-request-id", "not valid")]).await;
    let request_id = response
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();
    assert_ne!(request_id, "not valid");
    assert!(Uuid::parse_str(request_id).is_ok());
}

#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for SharedWriter {
    type Writer = SharedWriter;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn access_log_records_the_request_id_and_response_fields() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let writer = SharedWriter(captured.clone());
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .try_init()
        .ok();

    let app = support::app(support::state_with_pivotal());
    let response = get(
        &app,
        "/version",
        &[("x-request-id", "access-log-request-id")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let logs = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
    let line = logs
        .lines()
        .find(|line| line.contains("access-log-request-id"))
        .unwrap_or_else(|| panic!("request log line missing from {logs:?}"));
    assert!(line.contains("http request"), "log line: {line}");
    assert!(line.contains("method=GET"), "log line: {line}");
    assert!(line.contains("path=/version"), "log line: {line}");
    assert!(line.contains("status=200"), "log line: {line}");
    assert!(line.contains("latency_ms="), "log line: {line}");
}

// ---------------------------------------------------------------------------
// Forwarded headers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn trusted_proxy_uses_the_first_forwarded_values() {
    let state = state_with_trusted_proxies(&["127.0.0.1"]);
    let app = support::app(state);

    let url = listed_users_url(
        &app,
        Some("internal.example:8000"),
        Some("proxy.example.com, ignored.example.com"),
        Some("https, http"),
        Some(SocketAddr::from(([127, 0, 0, 1], 50000))),
    )
    .await;

    assert_eq!(url, "https://proxy.example.com/users/alice");
}

#[tokio::test]
async fn trusted_cidr_range_is_matched() {
    let state = state_with_trusted_proxies(&["10.0.0.0/8"]);
    let app = support::app(state);

    let url = listed_users_url(
        &app,
        Some("internal.example:8000"),
        Some("proxy.example.com"),
        Some("https"),
        Some(SocketAddr::from(([10, 1, 2, 3], 50000))),
    )
    .await;

    assert_eq!(url, "https://proxy.example.com/users/alice");
}

#[tokio::test]
async fn untrusted_peer_ignores_forwarded_headers_and_prefers_host() {
    let state = state_with_trusted_proxies(&["127.0.0.1"]);
    let app = support::app(state);

    let url = listed_users_url(
        &app,
        Some("internal.example:8000"),
        Some("attacker.example.com"),
        Some("https"),
        Some(SocketAddr::from(([203, 0, 113, 9], 50000))),
    )
    .await;

    assert_eq!(url, "http://internal.example:8000/users/alice");
}

#[tokio::test]
async fn empty_trusted_proxy_list_ignores_forwarded_headers() {
    let state = state_with_trusted_proxies(&[]);
    let app = support::app(state);

    let url = listed_users_url(
        &app,
        Some("internal.example:8000"),
        Some("attacker.example.com"),
        Some("https"),
        Some(SocketAddr::from(([127, 0, 0, 1], 50000))),
    )
    .await;

    assert_eq!(url, "http://internal.example:8000/users/alice");
}

#[tokio::test]
async fn malformed_forwarded_values_fall_back_to_host_and_listener_scheme() {
    let state = state_with_trusted_proxies(&["127.0.0.1"]);
    let app = support::app(state);

    let url = listed_users_url(
        &app,
        Some("internal.example:8000"),
        Some(", ignored.example.com"),
        Some("gopher"),
        Some(SocketAddr::from(([127, 0, 0, 1], 50000))),
    )
    .await;

    assert_eq!(url, "http://internal.example:8000/users/alice");
}

#[tokio::test]
async fn forwarded_host_with_userinfo_is_rejected() {
    let state = state_with_trusted_proxies(&["127.0.0.1"]);
    let app = support::app(state);

    let url = listed_users_url(
        &app,
        Some("internal.example:8000"),
        Some("attacker@proxy.example.com"),
        Some("https"),
        Some(SocketAddr::from(([127, 0, 0, 1], 50000))),
    )
    .await;

    // The malformed host is dropped independently; the valid proto remains.
    assert_eq!(url, "https://internal.example:8000/users/alice");
}

// ---------------------------------------------------------------------------
// Compression
// ---------------------------------------------------------------------------

async fn body_bytes(response: Response) -> Vec<u8> {
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

fn gunzip(bytes: &[u8]) -> String {
    let mut decoder = GzDecoder::new(bytes);
    let mut decoded = String::new();
    decoder.read_to_string(&mut decoded).unwrap();
    decoded
}

fn inflate(bytes: &[u8]) -> String {
    let mut decoder = ZlibDecoder::new(bytes);
    let mut decoded = String::new();
    decoder.read_to_string(&mut decoded).unwrap();
    decoded
}

#[tokio::test]
async fn compresses_text_responses_for_gzip_clients() {
    let app = support::app(support::state_with_pivotal());

    let identity = get(&app, "/version", &[]).await;
    assert!(
        identity.headers().get(header::CONTENT_ENCODING).is_none(),
        "identity response must stay uncompressed"
    );
    let identity_body = body_bytes(identity).await;

    let compressed = get(&app, "/version", &[("accept-encoding", "gzip")]).await;
    assert_eq!(
        compressed.headers().get(header::CONTENT_ENCODING).unwrap(),
        "gzip"
    );
    assert_eq!(
        compressed.headers().get(header::VARY).unwrap(),
        "Accept-Encoding"
    );
    let decoded = gunzip(&body_bytes(compressed).await);
    assert_eq!(decoded.as_bytes(), identity_body.as_slice());
    assert!(decoded.contains("Package: microwave-server/"));
}

#[tokio::test]
async fn negotiates_deflate_when_gzip_is_not_available() {
    let app = support::app(support::state_with_pivotal());
    let compressed = get(&app, "/version", &[("accept-encoding", "deflate")]).await;
    assert_eq!(
        compressed.headers().get(header::CONTENT_ENCODING).unwrap(),
        "deflate"
    );
    let decoded = inflate(&body_bytes(compressed).await);
    assert!(decoded.contains("Package: microwave-server/"));
}

#[tokio::test]
async fn does_not_compress_when_negotiation_disables_it() {
    let app = support::app(support::state_with_pivotal());

    for accept_encoding in ["gzip;q=0", "br", "identity"] {
        let response = get(&app, "/version", &[("accept-encoding", accept_encoding)]).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers().get(header::CONTENT_ENCODING).is_none(),
            "unexpected encoding for {accept_encoding:?}"
        );
        assert_eq!(
            response.headers().get(header::VARY).unwrap(),
            "Accept-Encoding"
        );
    }
}

#[tokio::test]
async fn compresses_json_responses() {
    let mut config = Config::default();
    support::disable_keygen(&mut config);
    let app = support::app(support::state_with_config(config));

    let response = get(&app, "/server_api_version", &[("accept-encoding", "gzip")]).await;
    assert_eq!(
        response.headers().get(header::CONTENT_ENCODING).unwrap(),
        "gzip"
    );
    let decoded = gunzip(&body_bytes(response).await);
    let value: Value = serde_json::from_str(&decoded).unwrap();
    assert_eq!(value["min_api_version"], 0);
}
