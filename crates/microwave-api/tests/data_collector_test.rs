mod support;

use axum::body::{to_bytes, Body};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::Response;
use axum::Router;
use microwave_config::Config;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tower::ServiceExt;

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

#[derive(Clone)]
struct MockState {
    status: u16,
    body: String,
    delay: Duration,
    captured: Arc<Mutex<Vec<CapturedRequest>>>,
}

async fn mock_handler(
    axum::extract::State(state): axum::extract::State<MockState>,
    request: Request<Body>,
) -> Response {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, usize::MAX)
        .await
        .unwrap_or_default()
        .to_vec();
    state.captured.lock().await.push(CapturedRequest {
        method: parts.method.to_string(),
        path: parts.uri.path().to_string(),
        headers: parts.headers,
        body,
    });
    if !state.delay.is_zero() {
        tokio::time::sleep(state.delay).await;
    }
    let mut response = Response::new(Body::from(state.body.clone()));
    *response.status_mut() = StatusCode::from_u16(state.status).unwrap();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

async fn start_mock(
    status: u16,
    body: &str,
    delay: Duration,
) -> (String, Arc<Mutex<Vec<CapturedRequest>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let state = MockState {
        status,
        body: body.to_string(),
        delay,
        captured: captured.clone(),
    };
    let app = Router::new().fallback(mock_handler).with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/data-collector/v0/"), captured)
}

/// Send response headers and a partial body immediately, then stall. This
/// exercises the response-body read timeout rather than the connect/headers
/// timeout covered by `start_mock`.
async fn start_slow_body_upstream(stall: Duration) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        read_request_headers(&stream).await;
        write_all(
            &stream,
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{\"partial\":",
        )
        .await;
        tokio::time::sleep(stall).await;
    });
    format!("http://{address}/data-collector/v0/")
}

async fn read_request_headers(stream: &tokio::net::TcpStream) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.try_read(&mut buffer) {
            Ok(0) => return,
            Ok(count) => request.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if stream.readable().await.is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

async fn write_all(stream: &tokio::net::TcpStream, data: &[u8]) {
    let mut written = 0;
    while written < data.len() {
        match stream.try_write(&data[written..]) {
            Ok(0) => return,
            Ok(count) => written += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if stream.writable().await.is_err() {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

fn data_collector_config(root_url: &str, token: Option<&str>, timeout: u64) -> Config {
    let mut config = Config::default();
    config.data_collector.root_url = Some(root_url.to_string());
    config.data_collector.token = token.map(ToString::to_string);
    config.data_collector.timeout = timeout;
    config
}

async fn app_with_data_collector(config: Config) -> axum::Router {
    let state = support::state_with_config(config);
    support::add_org(&state, "acme").await;
    support::app(state)
}

fn token_request(path: &str, token: &str, body: &'static [u8]) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .header("x-data-collector-token", token)
        .header("x-data-collector-auth", "version=1.0")
        .body(Body::from(body))
        .unwrap()
}

fn signed_post(path: &str, body: &'static [u8]) -> Request<Body> {
    support::signed_request(
        "POST",
        path,
        body,
        microwave_crypto::SignVersion::V1_3,
        "pivotal",
    )
}

async fn body_bytes(response: Response) -> Vec<u8> {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn disabled_data_collector_returns_404_for_signed_and_token_requests() {
    let app = support::app(support::state_with_pivotal());

    let response = app
        .clone()
        .oneshot(signed_post(
            "/organizations/acme/data-collector",
            br#"{"message":"run_start"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "any-token",
            br#"{"message":"run_start"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn token_request_is_forwarded_and_upstream_response_is_preserved() {
    let (root_url, captured) = start_mock(202, r#"{"status":"accepted"}"#, Duration::ZERO).await;
    let app = app_with_data_collector(data_collector_config(
        &root_url,
        Some("server-token"),
        5_000,
    ))
    .await;

    let payload = br#"{"message":"run_start","node":"web1"}"#;
    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "server-token",
            payload,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    assert_eq!(
        body_bytes(response).await,
        br#"{"status":"accepted"}"#.to_vec()
    );

    let captured = captured.lock().await;
    assert_eq!(captured.len(), 1);
    let forwarded = &captured[0];
    assert_eq!(forwarded.method, "POST");
    assert_eq!(forwarded.path, "/data-collector/v0/");
    assert_eq!(forwarded.body, payload);
    assert_eq!(
        forwarded
            .headers
            .get("x-data-collector-token")
            .unwrap()
            .to_str()
            .unwrap(),
        "server-token"
    );
    assert_eq!(
        forwarded
            .headers
            .get("x-data-collector-auth")
            .unwrap()
            .to_str()
            .unwrap(),
        "version=1.0"
    );
}

#[tokio::test]
async fn signed_request_is_forwarded_with_the_server_side_token() {
    let (root_url, captured) = start_mock(200, r#"{"ok":true}"#, Duration::ZERO).await;
    let app = app_with_data_collector(data_collector_config(
        &root_url,
        Some("server-token"),
        5_000,
    ))
    .await;

    let payload = br#"{"message":"run_end"}"#;
    let response = app
        .oneshot(signed_post("/organizations/acme/data-collector", payload))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let captured = captured.lock().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0]
            .headers
            .get("x-data-collector-token")
            .unwrap()
            .to_str()
            .unwrap(),
        "server-token"
    );
    assert_eq!(captured[0].body, payload);
}

#[tokio::test]
async fn invalid_token_without_a_signature_is_rejected_without_forwarding() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let app = app_with_data_collector(data_collector_config(
        &root_url,
        Some("server-token"),
        5_000,
    ))
    .await;

    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "wrong-token",
            br#"{}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(captured.lock().await.is_empty());
}

#[tokio::test]
async fn upstream_error_status_and_body_are_preserved() {
    let (root_url, _) = start_mock(422, r#"{"error":["payload rejected"]}"#, Duration::ZERO).await;
    let app = app_with_data_collector(data_collector_config(
        &root_url,
        Some("server-token"),
        5_000,
    ))
    .await;

    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "server-token",
            br#"{}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body_bytes(response).await,
        br#"{"error":["payload rejected"]}"#.to_vec()
    );
}

#[tokio::test]
async fn timeout_returns_gateway_timeout() {
    let (root_url, _) = start_mock(200, "{}", Duration::from_millis(250)).await;
    let app =
        app_with_data_collector(data_collector_config(&root_url, Some("server-token"), 10)).await;

    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "server-token",
            br#"{}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn slow_response_body_returns_gateway_timeout() {
    let root_url = start_slow_body_upstream(Duration::from_millis(1_000)).await;
    let app =
        app_with_data_collector(data_collector_config(&root_url, Some("server-token"), 200)).await;

    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "server-token",
            br#"{}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
}

#[tokio::test]
async fn oversized_upstream_response_returns_bad_gateway() {
    let oversized = "x".repeat(1_048_577);
    let (root_url, _) = start_mock(200, &oversized, Duration::ZERO).await;
    let app = app_with_data_collector(data_collector_config(
        &root_url,
        Some("server-token"),
        5_000,
    ))
    .await;

    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "server-token",
            br#"{}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn token_request_is_forwarded_without_an_org_lookup() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let app = app_with_data_collector(data_collector_config(
        &root_url,
        Some("server-token"),
        5_000,
    ))
    .await;

    let response = app
        .oneshot(token_request(
            "/organizations/missing/data-collector",
            "server-token",
            br#"{}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(captured.lock().await.len(), 1);
}

#[tokio::test]
async fn configured_data_collector_rejects_non_post_before_authentication() {
    let (root_url, _) = start_mock(200, "{}", Duration::ZERO).await;
    let app = app_with_data_collector(data_collector_config(
        &root_url,
        Some("server-token"),
        5_000,
    ))
    .await;

    // A token is not required to get the method-not-allowed response.
    let without_token = app
        .clone()
        .oneshot(
            Request::get("/organizations/acme/data-collector")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(without_token.status(), StatusCode::METHOD_NOT_ALLOWED);

    let with_token = app
        .oneshot(
            Request::put("/organizations/acme/data-collector")
                .header("x-data-collector-token", "server-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(with_token.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn token_config_is_optional_for_signed_requests() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let app = app_with_data_collector(data_collector_config(&root_url, None, 5_000)).await;

    let response = app
        .clone()
        .oneshot(signed_post(
            "/organizations/acme/data-collector",
            br#"{"message":"run_start"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(captured.lock().await.len(), 1);

    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "anything",
            br#"{}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn token_can_come_from_the_data_collector_secret() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let state = support::state_with_config(data_collector_config(&root_url, None, 5_000));
    support::add_org(&state, "acme").await;
    state
        .store
        .set_secret("data_collector", "token", "secret-token")
        .await
        .unwrap();
    let app = support::app(state);

    let response = app
        .oneshot(token_request(
            "/organizations/acme/data-collector",
            "secret-token",
            br#"{"message":"run_start"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let captured = captured.lock().await;
    assert_eq!(captured.len(), 1);
    assert_eq!(
        captured[0]
            .headers
            .get("x-data-collector-token")
            .unwrap()
            .to_str()
            .unwrap(),
        "secret-token"
    );
}

#[tokio::test]
async fn config_serializes_data_collector_and_telemetry_defaults() {
    let config = Config::default();
    let value = serde_json::to_value(&config).unwrap();
    assert_eq!(value["data_collector"]["timeout"], json!(30_000));
    assert!(value["data_collector"]["root_url"].is_null());
    assert_eq!(value["required_recipe"]["enabled"], json!(false));
    assert_eq!(value["telemetry"]["enabled"], json!(false));
}
