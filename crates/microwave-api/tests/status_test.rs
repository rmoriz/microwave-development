mod support;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_api::search::{
    IndexOutcome, SearchBackend, SearchDocument, SearchError, SearchHits, SearchRequest,
};
use microwave_objectstore::{ObjectStore, ObjectStoreError};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

async fn status_body(app: &axum::Router) -> (StatusCode, serde_json::Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/_status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (status, support::response_json(response).await)
}

#[tokio::test]
async fn status_reports_healthy_dependency_probes() {
    let app = support::app(support::state_with_pivotal());
    let (status, body) = status_body(&app).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "pong");
    assert_eq!(body["indexing"]["mode"], "embedded");
    assert!(body["keygen"].is_object());

    let upstreams = body["upstreams"]
        .as_object()
        .expect("upstreams is an upstream-compatible object");
    assert_eq!(upstreams.len(), 3);
    for name in ["chef_sql", "chef_index", "bookshelf"] {
        assert_eq!(upstreams[name], "pong", "{name} should be healthy");
        assert_eq!(body["upstream_details"][name]["status"], "pong");
        assert!(body["upstream_details"][name]["detail"].is_string());
    }
}

#[tokio::test]
async fn status_reports_a_failing_search_backend_without_leaking_credentials() {
    let mut state = support::state_with_pivotal();
    state.search = Arc::new(FailingSearch);
    let app = support::app(state);

    let (status, body) = status_body(&app).await;

    // A dependency failure is reported in the payload, never as a 500.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "fail");
    assert_eq!(body["upstreams"]["chef_index"], "fail");
    assert_eq!(body["upstreams"]["chef_sql"], "pong");
    assert_eq!(body["upstreams"]["bookshelf"], "pong");

    let detail = body["upstream_details"]["chef_index"]["detail"]
        .as_str()
        .unwrap();
    assert_eq!(detail, "backend returned HTTP 401");
    assert!(!detail.contains("secret"));
    assert!(!detail.contains("user@"));
    // Keygen and indexing are preserved on a degraded status response.
    assert!(body["keygen"].is_object());
    assert_eq!(body["indexing"]["mode"], "embedded");
}

#[tokio::test]
async fn status_reports_a_failing_object_store() {
    let mut state = support::state_with_pivotal();
    state.object_store = Arc::new(FailingObjectStore);
    let app = support::app(state);

    let (status, body) = status_body(&app).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "fail");
    assert_eq!(body["upstreams"]["bookshelf"], "fail");
    assert_eq!(body["upstreams"]["chef_sql"], "pong");
    assert_eq!(body["upstreams"]["chef_index"], "pong");
    assert!(body["upstream_details"]["bookshelf"]["detail"]
        .as_str()
        .unwrap()
        .contains("object store is unavailable"));
}

#[tokio::test]
async fn status_bounds_a_hanging_search_probe() {
    let mut state = support::state_with_pivotal();
    state.search = Arc::new(HangingSearch);
    let app = support::app(state);

    let started = std::time::Instant::now();
    let (status, body) = status_body(&app).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "fail");
    assert_eq!(body["upstreams"]["chef_index"], "fail");
    assert_eq!(
        body["upstream_details"]["chef_index"]["detail"],
        "probe timed out"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "health probe exceeded its bound: {:?}",
        started.elapsed()
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
            status: 401,
            url: "https://user:secret@opensearch.example.test/chef".to_string(),
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

struct HangingSearch;

#[async_trait]
impl SearchBackend for HangingSearch {
    fn name(&self) -> &'static str {
        "embedded"
    }

    async fn health_check(&self) -> Result<(), SearchError> {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(())
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

struct FailingObjectStore;

impl ObjectStore for FailingObjectStore {
    fn put(
        &self,
        _organization_id: &str,
        _checksum: &str,
        _data: &[u8],
    ) -> Result<(), ObjectStoreError> {
        unreachable!()
    }

    fn get(&self, _organization_id: &str, _checksum: &str) -> Result<Vec<u8>, ObjectStoreError> {
        unreachable!()
    }

    fn exists(&self, _organization_id: &str, _checksum: &str) -> bool {
        false
    }

    fn delete(&self, _organization_id: &str, _checksum: &str) -> Result<bool, ObjectStoreError> {
        unreachable!()
    }

    fn presigned_url(&self, _method: &str, _organization_id: &str, _checksum: &str) -> String {
        unreachable!()
    }

    fn health_check(&self) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::InvalidConfig(
            "object store is unavailable".to_string(),
        ))
    }
}
