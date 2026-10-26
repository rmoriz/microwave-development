mod support;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use microwave_api::search::{
    IndexOutcome, SearchBackend, SearchDocument, SearchError, SearchHits, SearchRequest,
};
use microwave_config::Config;
use std::sync::Arc;
use tower::ServiceExt;

const BASIC_ADMIN_SECRET: &str = "Basic YWRtaW46c2VjcmV0";
const BASIC_ADMIN_WRONG: &str = "Basic YWRtaW46d3Jvbmc=";

async fn get(app: &axum::Router, path: &str) -> axum::response::Response {
    app.clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn get_with_authorization(
    app: &axum::Router,
    path: &str,
    authorization: &str,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(header::AUTHORIZATION, authorization)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_text(response: axum::response::Response) -> String {
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}

#[tokio::test]
async fn unhealthy_status_returns_500_with_the_same_document() {
    let mut state = support::state_with_pivotal();
    state.search = Arc::new(FailingSearch);
    let app = support::app(state);

    let response = get(&app, "/_status").await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let body = support::response_json(response).await;
    assert_eq!(body["status"], "fail");
    assert_eq!(body["upstreams"]["chef_index"], "fail");
    assert_eq!(body["upstreams"]["chef_sql"], "pong");
    assert_eq!(body["upstreams"]["bookshelf"], "pong");
    assert!(body["upstream_details"]["chef_index"]["detail"].is_string());
    assert!(body["keygen"].is_object());
    assert_eq!(body["indexing"]["mode"], "embedded");
}

#[tokio::test]
async fn healthy_status_stays_200() {
    let app = support::app(support::state_with_pivotal());

    let response = get(&app, "/_status").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = support::response_json(response).await;
    assert_eq!(body["status"], "pong");
}

#[tokio::test]
async fn stats_text_format_returns_prometheus_text() {
    let app = support::app(support::state_with_pivotal());

    for path in ["/_stats?format=text", "/_stats?format=prometheus"] {
        let response = get(&app, path).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.starts_with("text/plain"),
            "{path} returned {content_type}"
        );
        let body = body_text(response).await;
        assert!(body.contains("# TYPE microwave_server_requests_total counter"));
        assert!(body.contains("microwave_server_requests_total "));
        assert!(body.contains("microwave_server_nodes "));
    }
}

#[tokio::test]
async fn stats_json_format_stays_json() {
    let app = support::app(support::state_with_pivotal());

    let response = get(&app, "/_stats?format=json").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    let body = support::response_json(response).await;
    assert!(body["requests"]["total"].is_u64());
}

#[tokio::test]
async fn stats_unknown_format_returns_406() {
    let app = support::app(support::state_with_pivotal());

    let response = get(&app, "/_stats?format=xml").await;
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
}

#[tokio::test]
async fn stats_basic_auth_protects_only_the_stats_endpoint() {
    let mut config = Config::default();
    config.stats.username = Some("admin".into());
    config.stats.password = Some("secret".into());
    let app = support::app(support::state_with_config(config));

    let response = get(&app, "/_stats?format=text").await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .unwrap()
            .to_str()
            .unwrap(),
        "Basic realm=\"Chef-Server\""
    );

    let response = get_with_authorization(&app, "/_stats?format=text", BASIC_ADMIN_WRONG).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(response.headers().contains_key(header::WWW_AUTHENTICATE));

    let response = get_with_authorization(&app, "/_stats?format=text", BASIC_ADMIN_SECRET).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_text(response)
        .await
        .contains("# TYPE microwave_server_requests_total counter"));

    // `/_status` does not require the stats credentials.
    let response = get(&app, "/_status").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(support::response_json(response).await["status"], "pong");
}

#[tokio::test]
async fn version_returns_an_unauthenticated_plain_text_manifest() {
    let app = support::app(support::state_with_pivotal());

    let response = get(&app, "/version").await;
    assert_eq!(response.status(), StatusCode::OK);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(content_type.starts_with("text/plain"), "{content_type}");

    let body = body_text(response).await;
    assert!(body.starts_with("Package: "), "{body}");
    assert!(
        body.contains(env!("CARGO_PKG_VERSION")),
        "manifest does not include the crate version: {body}"
    );
}

struct FailingSearch;

#[async_trait]
impl SearchBackend for FailingSearch {
    fn name(&self) -> &'static str {
        "embedded"
    }

    async fn health_check(&self) -> Result<(), SearchError> {
        Err(SearchError::Backend {
            status: 503,
            url: "https://search.example.test/chef".to_string(),
        })
    }

    async fn search(&self, _request: &SearchRequest) -> Result<SearchHits, SearchError> {
        unreachable!()
    }

    async fn index(&self, _document: &SearchDocument) -> Result<IndexOutcome, SearchError> {
        unreachable!()
    }

    async fn delete(&self, _org_id: &str, _id: &str) -> Result<(), SearchError> {
        unreachable!()
    }

    async fn delete_index(&self, _org_id: &str) -> Result<(), SearchError> {
        unreachable!()
    }
}
