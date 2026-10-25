mod support;

use axum::body::{to_bytes, Body};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::Response;
use axum::Router;
use microwave_config::Config;
use microwave_crypto::SignVersion;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tower::ServiceExt;

#[derive(Clone, Debug)]
struct CapturedRequest {
    method: String,
    path: String,
    query: Option<String>,
    headers: HeaderMap,
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
    let (parts, _) = request.into_parts();
    state.captured.lock().await.push(CapturedRequest {
        method: parts.method.to_string(),
        path: parts.uri.path().to_string(),
        query: parts.uri.query().map(ToString::to_string),
        headers: parts.headers,
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
    // Ensure the listener has accepted at least one connection before the
    // first assertion; this removes cold-start scheduling races.
    let _ = tokio::net::TcpStream::connect(address).await;
    (format!("http://{address}"), captured)
}

fn proxy_config(url: &str, token: Option<&str>, timeout: u64) -> Config {
    let mut config = Config::default();
    config.compliance_proxy.url = Some(url.to_string());
    config.compliance_proxy.token = token.map(ToString::to_string);
    config.compliance_proxy.timeout = timeout;
    config
}

async fn app_with_proxy(config: Config) -> axum::Router {
    let state = support::state_with_config(config);
    support::add_org(&state, "acme").await;
    support::app(state)
}

fn signed_get(path: &str) -> Request<Body> {
    support::signed_request("GET", path, b"", SignVersion::V1_3, "pivotal")
}

async fn wait_for(
    captured: &Arc<Mutex<Vec<CapturedRequest>>>,
    count: usize,
) -> Vec<CapturedRequest> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let requests = captured.lock().await.clone();
        if requests.len() >= count {
            return requests;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {count} proxy requests; saw {}",
            requests.len()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn body_bytes(response: Response) -> Vec<u8> {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

#[tokio::test]
async fn disabled_proxy_returns_404_for_both_route_spellings() {
    let app = app_with_proxy(Config::default()).await;

    for path in [
        "/organizations/acme/owners/foobar/compliance",
        "/organizations/acme/owners/foobar/compliance/testprofile",
        "/compliance/organizations/acme/owners/foobar/compliance",
        "/compliance/organizations/acme/owners/foobar/compliance/testprofile",
    ] {
        let response = app.clone().oneshot(signed_get(path)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        // The disabled surface is absent before authentication, matching the
        // optional required_recipe endpoint.
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn configured_proxy_rejects_non_get_before_authentication() {
    let (url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let app = app_with_proxy(proxy_config(&url, None, 10_000)).await;

    let response = app
        .oneshot(
            Request::post("/organizations/acme/owners/foobar/compliance")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(captured.lock().await.is_empty());
}

#[tokio::test]
async fn organization_scoped_route_rewrites_and_sets_service_headers() {
    let (url, captured) = start_mock(200, r#"{"response":"ok"}"#, Duration::ZERO).await;
    let app = app_with_proxy(proxy_config(&url, Some("service-token"), 10_000)).await;

    let response = app
        .oneshot(signed_get("/organizations/acme/owners/foobar/compliance"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    assert_eq!(body_bytes(response).await, br#"{"response":"ok"}"#.to_vec());

    let captured = wait_for(&captured, 1).await;
    let forwarded = &captured[0];
    assert_eq!(forwarded.method, "GET");
    assert_eq!(forwarded.path, "/compliance/profiles/foobar");
    assert_eq!(
        forwarded
            .headers
            .get("x-data-collector-token")
            .unwrap()
            .to_str()
            .unwrap(),
        "service-token"
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
    // Client Chef signatures and other request auth headers must not leak.
    assert!(!forwarded.headers.contains_key("x-ops-authorization-1"));
    assert!(!forwarded.headers.contains_key("x-ops-sign"));
}

#[tokio::test]
async fn legacy_compliance_prefix_and_profiles_are_forwarded() {
    let (url, captured) = start_mock(200, r#"{"response":"ok"}"#, Duration::ZERO).await;
    let app = app_with_proxy(proxy_config(&url, None, 10_000)).await;

    let response = app
        .clone()
        .oneshot(signed_get(
            "/compliance/organizations/acme/owners/foobar/compliance/testprofile?refresh=true",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(signed_get(
            "/organizations/acme/owners/foobar/compliance/base/linux",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let captured = wait_for(&captured, 2).await;
    assert_eq!(captured[0].path, "/compliance/profiles/foobar/testprofile");
    assert_eq!(captured[0].query.as_deref(), Some("refresh=true"));
    assert_eq!(captured[1].path, "/compliance/profiles/foobar/base/linux");
    // No token configured and no data collector token available, so no token
    // header is sent.
    assert!(!captured[0].headers.contains_key("x-data-collector-token"));
}

#[tokio::test]
async fn upstream_status_and_body_are_preserved() {
    let (url, _) = start_mock(404, r#"{"error":"profile missing"}"#, Duration::ZERO).await;
    let app = app_with_proxy(proxy_config(&url, None, 10_000)).await;

    let response = app
        .oneshot(signed_get(
            "/organizations/acme/owners/foobar/compliance/missing",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        body_bytes(response).await,
        br#"{"error":"profile missing"}"#.to_vec()
    );
}

#[tokio::test]
async fn traversal_segments_are_rejected_without_forwarding() {
    let (url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let app = app_with_proxy(proxy_config(&url, Some("service-token"), 10_000)).await;

    for path in [
        "/organizations/acme/owners/%2E%2E/compliance",
        "/organizations/acme/owners/%2e%2e/compliance",
        "/organizations/acme/owners/foo%2F..%2Fbar/compliance",
        "/organizations/acme/owners/foobar/compliance/%2E%2E/admin",
        "/organizations/acme/owners/foobar/compliance/base%2F..%2F..%2Fadmin",
        "/compliance/organizations/acme/owners/%2E%2E/compliance",
        "/compliance/organizations/acme/owners/foobar/compliance/%2E%2E/admin",
    ] {
        let response = app.clone().oneshot(signed_get(path)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
    assert!(captured.lock().await.is_empty());
}

#[tokio::test]
async fn timeout_returns_gateway_timeout() {
    let (url, _) = start_mock(200, "{}", Duration::from_millis(300)).await;
    let app = app_with_proxy(proxy_config(&url, None, 20)).await;

    let started = Instant::now();
    let response = app
        .oneshot(signed_get("/organizations/acme/owners/foobar/compliance"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(started.elapsed() < Duration::from_millis(1_000));
}

#[tokio::test]
async fn custom_token_header_is_used_when_configured() {
    let (url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let mut config = proxy_config(&url, Some("custom-token"), 10_000);
    config.compliance_proxy.token_header = Some("x-automate-token".into());
    let app = app_with_proxy(config).await;

    let response = app
        .oneshot(signed_get("/organizations/acme/owners/foobar/compliance"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let captured = wait_for(&captured, 1).await;
    assert_eq!(
        captured[0]
            .headers
            .get("x-automate-token")
            .unwrap()
            .to_str()
            .unwrap(),
        "custom-token"
    );
    assert!(!captured[0].headers.contains_key("x-data-collector-token"));
}

#[tokio::test]
async fn compliance_token_falls_back_to_the_data_collector_secret() {
    let (url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let state = support::state_with_config(proxy_config(&url, None, 10_000));
    support::add_org(&state, "acme").await;
    state
        .store
        .set_secret("data_collector", "token", "shared-token")
        .await
        .unwrap();
    let app = support::app(state);

    let response = app
        .oneshot(signed_get("/organizations/acme/owners/foobar/compliance"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let captured = wait_for(&captured, 1).await;
    assert_eq!(
        captured[0]
            .headers
            .get("x-data-collector-token")
            .unwrap()
            .to_str()
            .unwrap(),
        "shared-token"
    );
}
