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

fn node_request_for(user: &str, org: &str, address: Option<&str>) -> Request<Body> {
    let mut request = support::signed_request(
        "GET",
        &format!("/organizations/{org}/nodes"),
        b"",
        SignVersion::V1_3,
        user,
    );
    if let Some(address) = address {
        peer(&mut request, address);
    }
    request
}

fn node_request(org: &str, address: Option<&str>) -> Request<Body> {
    node_request_for("pivotal", org, address)
}

async fn node_get_for(
    app: &Router,
    user: &str,
    org: &str,
    address: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(node_request_for(user, org, Some(address)))
        .await
        .unwrap()
}

fn validate_request(org: &str) -> Request<Body> {
    let mut request = support::signed_request_with_signed_path(
        "GET",
        &format!("/organizations/{org}/validate/organizations/{org}/nodes"),
        &format!("/organizations/{org}/nodes"),
        b"",
        SignVersion::V1_3,
        "pivotal",
    );
    peer(&mut request, "127.0.0.1");
    request
}

async fn node_get(app: &Router, org: &str, address: &str) -> axum::response::Response {
    app.clone()
        .oneshot(node_request(org, Some(address)))
        .await
        .unwrap()
}

async fn node_get_without_peer(app: &Router, org: &str) -> axum::response::Response {
    app.clone().oneshot(node_request(org, None)).await.unwrap()
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
    let response = node_get(&app, "acme", "127.0.0.1").await;
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
        node_get_without_peer(&app, "acme").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        node_get_without_peer(&app, "acme").await.status(),
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
        node_get(&app, "acme", "127.0.0.1").await.status(),
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
        node_get(&app, "acme", "203.0.113.1").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        node_get(&app, "acme", "203.0.113.1").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        node_get(&app, "acme", "203.0.113.2").await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn org_bucket_rejects_one_org_without_touching_another() {
    let mut config = config();
    config.limits.org.burst = 1;
    config.limits.org.rate_per_second = 1;
    config.limits.queue_timeout_ms = 0;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    create_org(&state, "acme").await;
    create_org(&state, "other").await;

    assert_eq!(
        node_get(&app, "acme", "127.0.0.1").await.status(),
        StatusCode::OK
    );
    let rejected = node_get(&app, "acme", "127.0.0.1").await;
    assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejected.headers()["retry-after"], "1");
    assert_eq!(
        node_get(&app, "other", "127.0.0.1").await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_org_override_raises_only_that_orgs_burst() {
    let mut config = config();
    config.limits.org.burst = 1;
    config.limits.org.rate_per_second = 1;
    config.limits.queue_timeout_ms = 0;
    config.limits.organizations.insert(
        "acme".to_string(),
        microwave_config::LimitsOrgOverride {
            burst: Some(3),
            ..Default::default()
        },
    );
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    create_org(&state, "acme").await;
    create_org(&state, "other").await;

    // The override is matched by name but booked on the internal org ID, so
    // the three requests below drain one bucket.
    for _ in 0..3 {
        assert_eq!(
            node_get(&app, "acme", "127.0.0.1").await.status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        node_get(&app, "acme", "127.0.0.1").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    // The default of one token still applies to the other org.
    assert_eq!(
        node_get(&app, "other", "127.0.0.1").await.status(),
        StatusCode::OK
    );
    assert_eq!(
        node_get(&app, "other", "127.0.0.1").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn exempt_paths_stay_reachable_with_a_trailing_slash() {
    let mut config = config();
    config.limits.global.in_flight = 1;
    config.limits.queue_timeout_ms = 0;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    let _held = state.limits.global().await.unwrap();

    for path in ["/_status/", "/_stats/", "/version/"] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_ne!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{path} must stay exempt"
        );
    }
}

#[tokio::test]
async fn a_trailing_slash_request_consumes_one_org_token() {
    let mut config = config();
    config.limits.org.burst = 1;
    config.limits.org.rate_per_second = 1;
    config.limits.queue_timeout_ms = 0;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    create_org(&state, "acme").await;

    // The router re-enters itself after normalizing the slash; the marker must
    // keep that second pass from taking a second token.
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/nodes/",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        node_get(&app, "acme", "127.0.0.1").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn disabled_limits_pass_the_bookshelf_gate() {
    let mut config = config();
    config.limits.enabled = false;
    config.limits.org.in_flight = 1;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    let contents = b"cookbook file";
    let checksum = microwave_objectstore::md5_hex(contents);
    let path = bookshelf_path(&checksum, "PUT");

    let _held = state.limits.org_by_key(BOOKSHELF_ORG, None).await.unwrap();
    assert_eq!(
        bookshelf_send(&app, "PUT", &path, contents).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_org_override_raises_only_that_orgs_in_flight_cap() {
    let mut config = config();
    config.limits.org.in_flight = 1;
    config.limits.queue_timeout_ms = 0;
    config.limits.organizations.insert(
        "acme".to_string(),
        microwave_config::LimitsOrgOverride {
            in_flight: Some(2),
            ..Default::default()
        },
    );
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    let acme = support::add_org(&state, "acme").await;
    let other = support::add_org(&state, "other").await;

    // The entry starts on the default cap and is retuned by the override. The
    // permit taken before the retune keeps counting on the old semaphore, so
    // two further permits fit before the cap of two is exhausted.
    let _default = state.limits.org_by_key(&acme.id, None).await.unwrap();
    let _first = state
        .limits
        .org_by_key(&acme.id, Some("acme"))
        .await
        .unwrap();
    let _second = state
        .limits
        .org_by_key(&acme.id, Some("acme"))
        .await
        .unwrap();
    assert_eq!(
        node_get(&app, "acme", "127.0.0.1").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    let _other = state.limits.org_by_key(&other.id, None).await.unwrap();
    assert_eq!(
        node_get(&app, "other", "127.0.0.1").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn principal_bucket_rejects_one_principal_without_touching_another() {
    let mut config = config();
    config.limits.principal.burst = 1;
    config.limits.principal.rate_per_second = 1;
    config.limits.queue_timeout_ms = 0;
    let state = support::state_with_config(config);
    let org = support::add_org(&state, "acme").await;
    let alice = support::add_user(&state, "alice", "alice@example.com", false);
    let bob = support::add_user(&state, "bob", "bob@example.com", false);
    support::associate(&state, &org, &alice);
    support::associate(&state, &org, &bob);
    let app = support::app(state.clone());

    assert_eq!(
        node_get_for(&app, "alice", "acme", "127.0.0.1")
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        node_get_for(&app, "alice", "acme", "127.0.0.1")
            .await
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        node_get_for(&app, "bob", "acme", "127.0.0.1")
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_unknown_org_stays_404_and_consumes_no_org_token() {
    let mut config = config();
    config.limits.org.burst = 1;
    config.limits.org.rate_per_second = 1;
    config.limits.queue_timeout_ms = 0;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    create_org(&state, "acme").await;

    assert_eq!(
        node_get(&app, "missing", "127.0.0.1").await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        node_get(&app, "acme", "127.0.0.1").await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn validate_consumes_an_org_token() {
    let mut config = config();
    config.limits.org.burst = 1;
    config.limits.org.rate_per_second = 1;
    config.limits.queue_timeout_ms = 0;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    create_org(&state, "acme").await;

    assert_eq!(
        app.clone()
            .oneshot(validate_request("acme"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone()
            .oneshot(validate_request("acme"))
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

const BOOKSHELF_ORG: &str = "bookshelf-limits";
const BOOKSHELF_SECRET: &str = "test-secret";

fn bookshelf_path(checksum: &str, method: &str) -> String {
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 900;
    let token = microwave_objectstore::bookshelf_token(
        BOOKSHELF_SECRET,
        method,
        BOOKSHELF_ORG,
        checksum,
        expires_at,
    );
    format!(
        "/bookshelf/organizations/{BOOKSHELF_ORG}/checksums/{checksum}?token={token}&exp={expires_at}"
    )
}

async fn bookshelf_send(
    app: &Router,
    method: &str,
    path: &str,
    body: &[u8],
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::from(body.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn a_busy_org_bucket_rejects_a_bookshelf_upload() {
    let mut config = config();
    config.limits.org.in_flight = 1;
    config.limits.queue_timeout_ms = 0;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    let contents = b"cookbook file";
    let checksum = microwave_objectstore::md5_hex(contents);
    let path = bookshelf_path(&checksum, "PUT");

    let held = state.limits.org_by_key(BOOKSHELF_ORG, None).await.unwrap();
    let rejected = bookshelf_send(&app, "PUT", &path, contents).await;
    assert_eq!(rejected.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(rejected.headers()["retry-after"], "1");

    drop(held);
    assert_eq!(
        bookshelf_send(&app, "PUT", &path, contents).await.status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_busy_org_bucket_rejects_a_bookshelf_download() {
    let mut config = config();
    config.limits.org.in_flight = 1;
    config.limits.queue_timeout_ms = 0;
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    let path = bookshelf_path(&microwave_objectstore::md5_hex(b"missing"), "GET");

    let _held = state.limits.org_by_key(BOOKSHELF_ORG, None).await.unwrap();
    assert_eq!(
        bookshelf_send(&app, "GET", &path, b"").await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}
