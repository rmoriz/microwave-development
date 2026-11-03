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

async fn body_text(response: axum::response::Response) -> String {
    String::from_utf8(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

/// A Chef client marker, so the landing page does not answer instead of the API.
async fn get_as_client(app: &axum::Router, path: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("x-chef-version", "15.0.0")
                .body(Body::empty())
                .unwrap(),
        )
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
async fn stats_json_omits_pool_without_a_pool() {
    let app = support::app(support::state_with_pivotal());

    let response = get(&app, "/_stats?format=json").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = support::response_json(response).await;
    assert_eq!(body["nodes"], 0);
    assert!(body.get("db_pool").is_none());
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
    assert!(!body.contains("microwave_server_db_pool_connections"));
    assert!(!body.contains("microwave_server_db_pool_connections_max"));

    let response = get(&app, "/_stats?format=prometheus").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8(body.to_vec())
        .unwrap()
        .contains("microwave_server_requests_total 3"));
}

#[tokio::test]
async fn stats_renders_request_histograms() {
    let app = support::app(support::state_with_pivotal());
    get_as_client(&app, "/organizations/acme/nodes").await;
    let response = get(&app, "/_stats?format=prometheus").await;
    let body = body_text(response).await;
    assert!(body.contains("# TYPE microwave_server_request_duration_seconds histogram"));
    assert!(body.contains("microwave_server_request_duration_seconds_count{route=\"/organizations/{organization}/nodes\",method=\"GET\",org=\"acme\",status=\"401\"} 1"));
    assert!(body.contains("le=\"+Inf\""));
    let sum = body
        .lines()
        .find(|line| line.starts_with("microwave_server_request_duration_seconds_sum{route="))
        .expect("a sum sample");
    let sum: f64 = sum.rsplit(' ').next().unwrap().parse().unwrap();
    assert!(sum > 0.0, "the recorded duration is summed");
    let labels = sum_labels(&body);
    let infinity = bucket_value(&body, &labels, "+Inf");
    assert_eq!(
        infinity,
        count_value(&body, &labels),
        "the +Inf slot is the count"
    );
}

fn sum_labels(body: &str) -> String {
    body.lines()
        .find_map(|line| {
            line.strip_prefix("microwave_server_request_duration_seconds_sum{")?
                .split_once("} ")
                .map(|(labels, _)| labels.to_string())
        })
        .expect("a sum sample")
}

fn bucket_value(body: &str, labels: &str, le: &str) -> u64 {
    let prefix =
        format!("microwave_server_request_duration_seconds_bucket{{{labels},le=\"{le}\"}} ");
    let line = body
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("bucket {le}"));
    line[prefix.len()..].parse().unwrap()
}

fn count_value(body: &str, labels: &str) -> u64 {
    let prefix = format!("microwave_server_request_duration_seconds_count{{{labels}}} ");
    let line = body
        .lines()
        .find(|line| line.starts_with(&prefix))
        .expect("a count sample");
    line[prefix.len()..].parse().unwrap()
}

#[tokio::test]
async fn collapsed_series_render_one_label_set_each() {
    let state = support::state_with_pivotal();
    for index in 0..=microwave_metrics::REQUEST_SERIES_LIMIT {
        state.metrics.record_http(
            &format!("/r{index}"),
            "GET",
            "_default",
            200,
            std::time::Duration::from_millis(1),
        );
    }
    let app = support::app(state);
    let body = body_text(get(&app, "/_stats?format=prometheus").await).await;
    let mut seen = std::collections::HashSet::new();
    for line in body
        .lines()
        .filter(|line| line.starts_with("microwave_server_request_duration_seconds_count{"))
    {
        let labels = line.split_once('{').unwrap().1.split_once("} ").unwrap().0;
        assert!(
            seen.insert(labels.to_string()),
            "duplicate label set: {labels}"
        );
    }
    assert!(body.contains("route=\"_other\""));
    // One 1 ms record: the sum is exact and `+Inf` equals the count.
    let labels = "route=\"/r0\",method=\"GET\",org=\"_default\",status=\"200\"";
    assert!(body.contains(&format!(
        "microwave_server_request_duration_seconds_sum{{{labels}}} 0.001"
    )));
    assert_eq!(bucket_value(&body, labels, "+Inf"), 1);
    assert_eq!(count_value(&body, labels), 1);
}

#[tokio::test]
async fn stats_json_reports_the_otel_state() {
    let app = support::app(support::state_with_pivotal());
    let body = support::response_json(get(&app, "/_stats?format=json").await).await;
    assert_eq!(body["otel"]["enabled"], false);
    assert!(body["otel"]["endpoint"].is_null());
    assert!(body["otel"]["last_export_error"].is_null());
    assert!(body["keygen"]["capacity"].is_number());
    assert!(body["upstreams"]["database"].is_string());
}
