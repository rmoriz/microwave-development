mod support;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use microwave_crypto::SignVersion;
use tower::ServiceExt;

async fn get(app: &axum::Router, path: &str) -> axum::response::Response {
    app.clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn maintenance_mode_returns_503_without_authentication() {
    let state = support::state_with_pivotal();
    state.sqlite().set_maintenance_enabled(true).unwrap();
    let app = support::app(state);

    let response = get(&app, "/server_api_version").await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let version_header = response
        .headers()
        .get("x-ops-server-api-version")
        .expect("version header is preserved on 503")
        .to_str()
        .unwrap();
    assert!(version_header.contains(r#""min_version":"0""#));
    let body = support::response_json(response).await;
    assert!(body["error"][0]
        .as_str()
        .unwrap()
        .contains("maintenance mode"));

    // A correctly signed request is rejected too; maintenance is checked
    // before signature verification.
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/license",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn maintenance_mode_keeps_status_and_stats_reachable() {
    let state = support::state_with_pivotal();
    state.sqlite().set_maintenance_enabled(true).unwrap();
    let app = support::app(state);

    let response = get(&app, "/_status").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(support::response_json(response).await["status"], "pong");

    let response = get(&app, "/_stats").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(support::response_json(response).await["maintenance"], true);

    let response = get(&app, "/_stats?format=prometheus").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("microwave_server_maintenance 1"));
}

#[tokio::test]
async fn stats_reports_json_counters() {
    let app = support::app(support::state_with_pivotal());

    // A request signed by an unknown client is rejected by authentication and
    // counted as a 401.
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/license",
            b"",
            SignVersion::V1_3,
            "missing-user",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // A valid signed request is counted as a 2xx.
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/license",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = get(&app, "/_stats").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    let body = support::response_json(response).await;
    assert_eq!(body["requests"]["total"], 3);
    assert_eq!(body["requests"]["responses"]["2xx"], 1);
    assert_eq!(body["requests"]["responses"]["4xx"], 1);
    assert_eq!(body["requests"]["responses"]["5xx"], 0);
    assert_eq!(body["requests"]["authentication_failures"], 1);
    assert_eq!(body["maintenance"], false);
    assert_eq!(body["nodes"], 0);
    assert!(body["uptime_seconds"].as_u64().is_some());
}

#[tokio::test]
async fn stats_renders_prometheus_text() {
    let app = support::app(support::state_with_pivotal());

    let response = get(&app, "/_stats").await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/_stats")
                .header(header::ACCEPT, "text/plain")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/plain"));
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("# TYPE microwave_server_requests_total counter"));
    assert!(body.contains("microwave_server_requests_total 2"));
    assert!(body.contains("microwave_server_responses_total{class=\"2xx\"} 1"));
    assert!(body.contains("microwave_server_responses_total{class=\"4xx\"} 0"));
    assert!(body.contains("microwave_server_authentication_failures_total 0"));
    assert!(body.contains("microwave_server_uptime_seconds "));
    assert!(body.contains("microwave_server_maintenance 0"));
    assert!(body.contains("microwave_server_nodes 0"));

    let response = get(&app, "/_stats?format=prometheus").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8(body.to_vec())
        .unwrap()
        .contains("microwave_server_requests_total 3"));
}
