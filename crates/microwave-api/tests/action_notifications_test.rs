mod support;

use axum::body::{to_bytes, Body};
use axum::http::{header, HeaderMap, HeaderValue, Request, StatusCode};
use axum::response::Response;
use axum::Router;
use microwave_config::Config;
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
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
    // Ensure the listener has accepted at least one connection before the
    // first assertion; this removes cold-start scheduling races.
    let _ = tokio::net::TcpStream::connect(address).await;
    (format!("http://{address}/data-collector/v0/"), captured)
}

fn action_config(root_url: &str, token: Option<&str>, timeout: u64) -> Config {
    let mut config = Config::default();
    config.data_collector.root_url = Some(root_url.to_string());
    config.data_collector.token = token.map(ToString::to_string);
    config.data_collector.timeout = timeout;
    config.data_collector.actions_enabled = true;
    config
}

async fn app_with_actions(config: Config) -> axum::Router {
    let state = support::state_with_config(config);
    support::add_org(&state, "acme").await;
    support::app(state)
}

fn signed(method: &str, path: &str, body: &[u8]) -> Request<Body> {
    support::signed_request(method, path, body, SignVersion::V1_3, "pivotal")
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
            "timed out waiting for {count} action notifications; saw {}",
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
async fn disabled_actions_send_nothing_even_when_root_url_is_configured() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let mut config = action_config(&root_url, Some("action-token"), 10_000);
    config.data_collector.actions_enabled = false;
    let app = app_with_actions(config).await;

    let response = app
        .oneshot(signed(
            "POST",
            "/organizations/acme/nodes",
            br#"{"name":"web1"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(captured.lock().await.is_empty());
}

#[tokio::test]
async fn node_create_sends_an_upstream_shaped_action_with_the_server_token() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let app = app_with_actions(action_config(&root_url, Some("action-token"), 10_000)).await;

    let mut request = signed(
        "POST",
        "/organizations/acme/nodes",
        br#"{"name":"web1","chef_environment":"production"}"#,
    );
    request
        .headers_mut()
        .insert("x-forwarded-for", HeaderValue::from_static("10.0.0.5"));
    request
        .headers_mut()
        .insert("user-agent", HeaderValue::from_static("knife/18.0"));
    request
        .headers_mut()
        .insert("x-request-id", HeaderValue::from_static("request-123"));
    request.headers_mut().insert(
        header::HOST,
        HeaderValue::from_static("chef.example.com:8000"),
    );

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let captured = wait_for(&captured, 1).await;
    let forwarded = &captured[0];
    assert_eq!(forwarded.method, "POST");
    assert_eq!(forwarded.path, "/data-collector/v0/");
    assert_eq!(
        forwarded
            .headers
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "application/json"
    );
    assert_eq!(
        forwarded
            .headers
            .get("x-data-collector-token")
            .unwrap()
            .to_str()
            .unwrap(),
        "action-token"
    );
    // Action messages use the upstream data_collector_http defaults, which do
    // not include the client-facing auth-version header.
    assert!(!forwarded.headers.contains_key("x-data-collector-auth"));

    let payload: Value = serde_json::from_slice(&forwarded.body).unwrap();
    assert_eq!(payload["message_type"], "action");
    assert_eq!(payload["message_version"], "0.1.1");
    assert_eq!(payload["organization_name"], "acme");
    assert_eq!(payload["service_hostname"], "chef.example.com");
    assert_eq!(payload["remote_hostname"], "10.0.0.5");
    assert_eq!(payload["request_id"], "request-123");
    assert_eq!(payload["requestor_name"], "pivotal");
    assert_eq!(payload["requestor_type"], "user");
    assert_eq!(payload["user_agent"], "knife/18.0");
    assert_eq!(payload["task"], "create");
    assert_eq!(payload["entity_type"], "node");
    assert_eq!(payload["entity_name"], "web1");
    assert_eq!(payload["data"]["name"], "web1");
    assert!(payload["id"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(payload["recorded_at"]
        .as_str()
        .is_some_and(|at| at.ends_with('Z')));
}

#[tokio::test]
async fn membership_association_accepts_the_user_body_field() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let state = support::state_with_config(action_config(&root_url, None, 10_000));
    support::add_org(&state, "acme").await;
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let response = app
        .oneshot(signed(
            "POST",
            "/organizations/acme/users",
            br#"{"user":"alice"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let captured = wait_for(&captured, 1).await;
    let payload: Value = serde_json::from_slice(&captured[0].body).unwrap();
    assert_eq!(payload["task"], "associate");
    assert_eq!(payload["entity_type"], "user");
    assert_eq!(payload["entity_name"], "alice");
}

#[tokio::test]
async fn large_body_with_stalled_collector_is_bounded_and_omits_data() {
    let (root_url, captured) = start_mock(200, "{}", Duration::from_secs(3)).await;
    let app = app_with_actions(action_config(&root_url, None, 10_000)).await;

    let mut body = br#"{"name":"web1","normal":{"blob":""#.to_vec();
    body.extend(std::iter::repeat_n(b'x', 300 * 1024));
    body.extend_from_slice(br#""}}"#);

    let started = Instant::now();
    let response = app
        .oneshot(signed("POST", "/organizations/acme/nodes", &body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "request path waited on a large notification"
    );

    let captured = wait_for(&captured, 1).await;
    assert!(
        captured[0].body.len() < 16 * 1024,
        "action retained/forwarded the full body: {} bytes",
        captured[0].body.len()
    );
    let payload: Value = serde_json::from_slice(&captured[0].body).unwrap();
    assert_eq!(payload["entity_type"], "node");
    assert_eq!(payload["entity_name"], "web1");
    assert!(payload.get("data").is_none());
}

#[tokio::test]
async fn dissociate_uses_the_path_user_and_ignores_a_spoofed_body() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let state = support::state_with_config(action_config(&root_url, None, 10_000));
    let org = support::add_org(&state, "acme").await;
    let user = support::add_user(&state, "alice", "alice@example.com", false);
    support::associate(&state, &org, &user);
    let app = support::app(state);

    let response = app
        .oneshot(signed(
            "DELETE",
            "/organizations/acme/users/alice",
            br#"{"user":"mallory"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let captured = wait_for(&captured, 1).await;
    let payload: Value = serde_json::from_slice(&captured[0].body).unwrap();
    assert_eq!(payload["task"], "dissociate");
    assert_eq!(payload["entity_type"], "user");
    assert_eq!(payload["entity_name"], "alice");
}

#[tokio::test]
async fn invitation_delete_reports_the_resolved_user() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let state = support::state_with_config(action_config(&root_url, None, 10_000));
    support::add_org(&state, "acme").await;
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let response = app
        .clone()
        .oneshot(signed(
            "POST",
            "/organizations/acme/association_requests",
            br#"{"username":"alice"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let invitation = support::response_json(response).await;
    let id = invitation["uri"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();

    // Let the invitation-create action arrive, then clear it so the delete is
    // the only notification under test.
    wait_for(&captured, 1).await;
    captured.lock().await.clear();

    let response = app
        .oneshot(signed(
            "DELETE",
            &format!("/organizations/acme/association_requests/{id}"),
            b"",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let captured = wait_for(&captured, 1).await;
    let payload: Value = serde_json::from_slice(&captured[0].body).unwrap();
    assert_eq!(payload["task"], "reject");
    assert_eq!(payload["entity_type"], "user");
    assert_eq!(payload["entity_name"], "alice");
}

#[tokio::test]
async fn create_update_delete_are_reported_with_the_expected_tasks() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let app = app_with_actions(action_config(&root_url, None, 10_000)).await;

    let response = app
        .clone()
        .oneshot(signed(
            "POST",
            "/organizations/acme/roles",
            br#"{"name":"web"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = app
        .clone()
        .oneshot(signed(
            "PUT",
            "/organizations/acme/roles/web",
            br#"{"name":"web","run_list":[]}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(signed("DELETE", "/organizations/acme/roles/web", b""))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let captured = wait_for(&captured, 3).await;
    let tasks: Vec<String> = captured
        .iter()
        .map(|request| {
            let payload: Value = serde_json::from_slice(&request.body).unwrap();
            payload["task"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(tasks, ["create", "update", "delete"]);
    for request in &captured {
        let payload: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(payload["entity_type"], "role");
        assert_eq!(payload["entity_name"], "web");
    }
}

#[tokio::test]
async fn failed_mutations_do_not_emit_actions() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let app = app_with_actions(action_config(&root_url, None, 10_000)).await;

    let response = app
        .oneshot(signed("POST", "/organizations/acme/nodes", br#"{}"#))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(captured.lock().await.is_empty());
}

#[tokio::test]
async fn a_slow_collector_does_not_delay_the_api_response() {
    let (root_url, captured) = start_mock(200, "{}", Duration::from_secs(5)).await;
    let app = app_with_actions(action_config(&root_url, None, 200)).await;

    let started = Instant::now();
    let response = app
        .oneshot(signed(
            "POST",
            "/organizations/acme/nodes",
            br#"{"name":"web1"}"#,
        ))
        .await
        .unwrap();
    let elapsed = started.elapsed();
    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(
        elapsed < Duration::from_secs(2),
        "request path waited on the collector: {elapsed:?}"
    );

    // The collector records the request before sleeping, so the notification
    // was dispatched even though the worker will time out waiting for it.
    wait_for(&captured, 1).await;
}

#[tokio::test]
async fn a_refused_collector_does_not_change_the_api_response() {
    // Port 9 is normally closed; the worker fails asynchronously.
    let app = app_with_actions(action_config("http://127.0.0.1:9/", None, 100)).await;
    let response = app
        .oneshot(signed(
            "POST",
            "/organizations/acme/nodes",
            br#"{"name":"web1"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_bytes(response).await;
    assert!(!body.is_empty());
}

#[tokio::test]
async fn action_token_can_come_from_the_data_collector_secret() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let state = support::state_with_config(action_config(&root_url, None, 10_000));
    support::add_org(&state, "acme").await;
    state
        .store
        .set_secret("data_collector", "token", "secret-action-token")
        .await
        .unwrap();
    let app = support::app(state);

    let response = app
        .oneshot(signed(
            "POST",
            "/organizations/acme/environments",
            br#"{"name":"production"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let captured = wait_for(&captured, 1).await;
    assert_eq!(
        captured[0]
            .headers
            .get("x-data-collector-token")
            .unwrap()
            .to_str()
            .unwrap(),
        "secret-action-token"
    );
}

#[tokio::test]
async fn token_lookup_retries_after_a_transient_missing_secret() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let state = support::state_with_config(action_config(&root_url, None, 10_000));
    support::add_org(&state, "acme").await;
    let app = support::app(state.clone());

    let response = app
        .clone()
        .oneshot(signed(
            "POST",
            "/organizations/acme/environments",
            br#"{"name":"production"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let captured_first = wait_for(&captured, 1).await;
    assert!(!captured_first[0]
        .headers
        .contains_key("x-data-collector-token"));

    state
        .store
        .set_secret("data_collector", "token", "late-token")
        .await
        .unwrap();

    let response = app
        .oneshot(signed(
            "POST",
            "/organizations/acme/environments",
            br#"{"name":"staging"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let captured = wait_for(&captured, 2).await;
    assert_eq!(
        captured[1]
            .headers
            .get("x-data-collector-token")
            .unwrap()
            .to_str()
            .unwrap(),
        "late-token"
    );
}

#[tokio::test]
async fn passwords_are_redacted_in_user_actions() {
    let (root_url, captured) = start_mock(200, "{}", Duration::ZERO).await;
    let app = app_with_actions(action_config(&root_url, None, 10_000)).await;

    let response = app
        .oneshot(signed(
            "POST",
            "/users",
            br#"{"username":"alice","display_name":"Alice","email":"alice@example.com","password":"hunter2"}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let captured = wait_for(&captured, 1).await;
    let payload: Value = serde_json::from_slice(&captured[0].body).unwrap();
    assert_eq!(payload["entity_type"], "user");
    assert_eq!(payload["entity_name"], "alice");
    assert_eq!(payload["data"]["password"], "*******");
    assert_eq!(payload["organization_name"], json!(null));
}
