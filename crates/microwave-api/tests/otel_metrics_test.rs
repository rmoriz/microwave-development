mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_config::Config;
use tower::ServiceExt;

async fn get(app: &axum::Router, path: &str) -> axum::response::Response {
    app.clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
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
async fn records_a_route_template_and_the_org() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    let response = get_as_client(&app, "/organizations/acme/nodes").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let series = state.metrics.http_series();
    let entry = series.first().expect("a recorded series");
    assert_eq!(entry.route, "/organizations/{organization}/nodes");
    assert_eq!(entry.org, "acme");
    assert_eq!(entry.status, 401);
}

/// The default-org rewrite re-enters the router through `authenticate`. The
/// re-entry must not record a second series for the same request.
#[tokio::test]
async fn default_org_rewrite_records_one_series() {
    let mut config = Config::default();
    config.server.default_orgname = Some("acme".into());
    let state = support::state_with_config(config);
    let app = support::app(state.clone());
    let response = get_as_client(&app, "/nodes").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let series = state.metrics.http_series();
    assert_eq!(
        series.len(),
        1,
        "the rewrite re-entry must count one request"
    );
    assert_eq!(series[0].route, "/organizations/{organization}/nodes");
    assert_eq!(series[0].status, 401);
    assert_eq!(series[0].count, 1);
}

#[tokio::test]
async fn records_unmatched_paths_with_the_normalizer() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    // No route matches, so the fallback has no template and the normalizer has
    // to supply the bounded label.
    get(&app, "/totally-unknown").await;
    let series = state.metrics.http_series();
    assert_eq!(series[0].route, "unknown");
    assert_eq!(series[0].org, "_default");
}

#[tokio::test]
async fn records_the_landing_page_as_its_own_route() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    let response = get(&app, "/").await;
    assert_eq!(response.status(), StatusCode::OK);
    let series = state.metrics.http_series();
    assert_eq!(series[0].route, "/");
    assert_eq!(series[0].org, "_default");
}
