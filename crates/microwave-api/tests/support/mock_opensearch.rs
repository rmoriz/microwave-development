//! Minimal in-process OpenSearch stub for integration tests.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Router;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct CapturedRequest {
    pub method: String,
    pub uri: String,
    pub body: String,
}

#[derive(Clone)]
struct MockState {
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    hits: Arc<Mutex<Vec<String>>>,
}

pub struct MockOpenSearch {
    pub base_url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    hits: Arc<Mutex<Vec<String>>>,
}

impl MockOpenSearch {
    /// Sets the IDs returned by subsequent `_search` requests.
    pub fn set_hits(&self, ids: &[String]) {
        *self.hits.lock().unwrap() = ids.to_vec();
    }

    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().unwrap().clone()
    }

    /// Returns the bodies of all `_bulk` requests seen so far.
    pub fn bulk_bodies(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|request| request.uri == "/_bulk")
            .map(|request| request.body)
            .collect()
    }
}

pub async fn start() -> MockOpenSearch {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let hits = Arc::new(Mutex::new(Vec::new()));
    let state = MockState {
        requests: requests.clone(),
        hits: hits.clone(),
    };
    let app = Router::new().fallback(handler).with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    MockOpenSearch {
        base_url: format!("http://{address}"),
        requests,
        hits,
    }
}

async fn handler(State(state): State<MockState>, request: Request) -> Response {
    let method = request.method().to_string();
    let uri = request.uri().to_string();
    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .unwrap();
    state.requests.lock().unwrap().push(CapturedRequest {
        method: method.clone(),
        uri: uri.clone(),
        body: String::from_utf8_lossy(&body).to_string(),
    });

    if method == "HEAD" && uri == "/chef" {
        return StatusCode::NOT_FOUND.into_response();
    }

    if uri.ends_with("/_cluster/health") {
        return axum::Json(json!({ "status": "green" })).into_response();
    }

    if uri.ends_with("/_search") {
        let hits = state.hits.lock().unwrap().clone();
        let hits: Vec<Value> = hits.iter().map(|id| json!({ "_id": id })).collect();
        return axum::Json(json!({
            "hits": {
                "total": { "value": hits.len(), "relation": "eq" },
                "hits": hits
            }
        }))
        .into_response();
    }

    if uri.ends_with("_delete_by_query") {
        return axum::Json(json!({ "deleted": 0 })).into_response();
    }

    axum::Json(json!({ "errors": false, "items": [] })).into_response()
}
