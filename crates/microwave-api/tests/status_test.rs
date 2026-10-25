mod support;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_api::search::{
    IndexOutcome, SearchBackend, SearchDocument, SearchError, SearchHits, SearchRequest,
};
use microwave_objectstore::{ObjectStore, ObjectStoreError};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_reports_busy_without_waiting_for_a_held_sqlite_lock() {
    let state = support::state_with_pivotal();
    let sqlite = state.sqlite.clone().expect("sqlite handle");
    let app = support::app(state);

    let (started_tx, started_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _guard = sqlite.lock_connection_for_test();
        started_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_secs(3));
    });
    started_rx.recv().unwrap();

    let started = Instant::now();
    let (status, body) = status_body(&app).await;
    let elapsed = started.elapsed();

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "fail");
    assert_eq!(body["upstreams"]["chef_sql"], "fail");
    assert!(
        body["upstream_details"]["chef_sql"]["detail"]
            .as_str()
            .unwrap()
            .contains("busy"),
        "unexpected detail: {}",
        body["upstream_details"]["chef_sql"]["detail"]
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "a held SQLite lock delayed /_status: {elapsed:?}"
    );

    holder.join().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_stale_requests_spawn_only_one_refresh() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut state = support::state_with_pivotal();
    state.search = Arc::new(CountingSearch {
        calls: Arc::clone(&calls),
        delay: Duration::from_millis(300),
    });
    let app = support::app(state);

    let (_, prime) = status_body(&app).await;
    assert_eq!(prime["status"], "pong");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    tokio::time::sleep(Duration::from_millis(1_100)).await;

    let mut tasks = Vec::new();
    for _ in 0..32 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move { status_body(&app).await }));
    }
    for task in tasks {
        let (status, body) = task.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "pong", "stale requests keep the snapshot");
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    while calls.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "a stale burst must create exactly one background refresh"
    );
}

#[tokio::test]
async fn status_reuses_the_cached_snapshot_and_refreshes_after_its_ttl() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut state = support::state_with_pivotal();
    state.search = Arc::new(CountingSearch {
        calls: Arc::clone(&calls),
        delay: Duration::ZERO,
    });
    let app = support::app(state);

    let (_, first) = status_body(&app).await;
    let (_, second) = status_body(&app).await;
    assert_eq!(first["status"], "pong");
    assert_eq!(second["status"], "pong");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the second request must reuse the cached snapshot"
    );

    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let (_, stale) = status_body(&app).await;
    assert_eq!(stale["status"], "pong", "stale-while-refresh keeps serving");

    let deadline = Instant::now() + Duration::from_secs(2);
    while calls.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        calls.load(Ordering::SeqCst) >= 2,
        "the expired snapshot was not refreshed in the background"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_status_requests_share_one_health_probe() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut state = support::state_with_pivotal();
    state.search = Arc::new(CountingSearch {
        calls: Arc::clone(&calls),
        delay: Duration::from_millis(100),
    });
    let app = support::app(state);

    let mut tasks = Vec::new();
    for _ in 0..16 {
        let app = app.clone();
        tasks.push(tokio::spawn(async move { status_body(&app).await }));
    }
    for task in tasks {
        let (status, body) = task.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "pong");
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a cold cache burst must run exactly one probe"
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

struct CountingSearch {
    calls: Arc<AtomicUsize>,
    delay: Duration,
}

#[async_trait]
impl SearchBackend for CountingSearch {
    fn name(&self) -> &'static str {
        "embedded"
    }

    async fn health_check(&self) -> Result<(), SearchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
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
