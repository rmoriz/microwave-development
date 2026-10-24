//! Integration tests for the S3 backend against a local mock HTTP server.
//!
//! No AWS, Garage, or Docker is required.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Router;
use microwave_objectstore::{ObjectStore, ObjectStoreError, S3Config, S3ObjectStore};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const HELLO: &[u8] = b"hello";
const HELLO_CHECKSUM: &str = "5d41402abc4b2a76b9719d911017c592";
const HELLO_MD5_B64: &str = "XUFAKrxLKna5cZ2REBfFkg==";

#[derive(Debug, Clone)]
struct Captured {
    method: String,
    path: String,
    query: Option<String>,
    headers: Vec<(String, String)>,
}

impl Captured {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone)]
struct MockState {
    objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    requests: Arc<Mutex<Vec<Captured>>>,
}

impl MockState {
    fn new() -> Self {
        Self {
            objects: Arc::new(Mutex::new(HashMap::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<Captured> {
        self.requests.lock().unwrap().clone()
    }

    fn last_request(&self) -> Captured {
        self.requests().pop().expect("a request was recorded")
    }
}

async fn handler(State(state): State<MockState>, request: Request) -> Response {
    let method = request.method().as_str().to_string();
    let path = request.uri().path().to_string();
    let query = request.uri().query().map(str::to_string);
    let headers = header_pairs(request.headers());
    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();

    state.requests.lock().unwrap().push(Captured {
        method: method.clone(),
        path: path.clone(),
        query,
        headers,
    });

    match method.as_str() {
        "PUT" => {
            state.objects.lock().unwrap().insert(path, body);
            StatusCode::OK.into_response()
        }
        "GET" => match state.objects.lock().unwrap().get(&path).cloned() {
            Some(data) => (StatusCode::OK, data).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        "HEAD" => {
            if state.objects.lock().unwrap().contains_key(&path) {
                StatusCode::OK.into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
        "DELETE" => {
            if state.objects.lock().unwrap().remove(&path).is_some() {
                StatusCode::NO_CONTENT.into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

fn header_pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

async fn start_mock() -> (String, MockState) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = MockState::new();
    let app = Router::new().fallback(handler).with_state(state.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}"), state)
}

fn store(endpoint: &str) -> Arc<S3ObjectStore> {
    Arc::new(
        S3ObjectStore::new(S3Config::new(
            endpoint,
            "chef",
            "garage",
            "AKIAEXAMPLE",
            "SECRETEXAMPLE",
        ))
        .unwrap(),
    )
}

async fn run_blocking<T: Send + 'static>(operation: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(operation).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_get_head_delete_round_trip_through_mock() {
    let (endpoint, mock) = start_mock().await;
    let store = store(&endpoint);

    let put_store = store.clone();
    run_blocking(move || put_store.put("acme", HELLO_CHECKSUM, HELLO))
        .await
        .unwrap();

    let captured = mock.last_request();
    assert_eq!(captured.method, "PUT");
    assert_eq!(
        captured.path,
        format!("/chef/organization-acme/checksum-{HELLO_CHECKSUM}")
    );
    assert_eq!(captured.header("content-md5"), Some(HELLO_MD5_B64));
    assert_eq!(
        captured.header("content-type"),
        Some("application/x-binary")
    );
    let expected_payload_hash = hex::encode(Sha256::digest(HELLO));
    assert_eq!(
        captured.header("x-amz-content-sha256"),
        Some(expected_payload_hash.as_str())
    );
    assert!(captured
        .header("authorization")
        .unwrap()
        .starts_with("AWS4-HMAC-SHA256 Credential=AKIAEXAMPLE/"));

    let get_store = store.clone();
    let data = run_blocking(move || get_store.get("acme", HELLO_CHECKSUM))
        .await
        .unwrap();
    assert_eq!(data, HELLO);
    let captured = mock.last_request();
    assert_eq!(captured.method, "GET");
    assert_eq!(
        captured.path,
        format!("/chef/organization-acme/checksum-{HELLO_CHECKSUM}")
    );

    let head_store = store.clone();
    assert!(run_blocking(move || head_store.exists("acme", HELLO_CHECKSUM)).await);
    assert_eq!(mock.last_request().method, "HEAD");

    let delete_store = store.clone();
    assert!(
        run_blocking(move || delete_store.delete("acme", HELLO_CHECKSUM))
            .await
            .unwrap()
    );
    assert_eq!(mock.last_request().method, "DELETE");

    let head_store = store.clone();
    assert!(!run_blocking(move || head_store.exists("acme", HELLO_CHECKSUM)).await);
    let get_store = store.clone();
    assert!(matches!(
        run_blocking(move || get_store.get("acme", HELLO_CHECKSUM)).await,
        Err(ObjectStoreError::NotFound)
    ));
    let delete_store = store;
    assert!(
        !run_blocking(move || delete_store.delete("acme", HELLO_CHECKSUM))
            .await
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_requests_are_sigv4_signed() {
    let (endpoint, mock) = start_mock().await;
    let store = store(&endpoint);

    run_blocking(move || store.put("acme", HELLO_CHECKSUM, HELLO))
        .await
        .unwrap();

    let captured = mock.last_request();
    let authorization = captured.header("authorization").expect("signed request");
    assert!(authorization.starts_with("AWS4-HMAC-SHA256 Credential=AKIAEXAMPLE/"));
    assert!(authorization.contains("/garage/s3/aws4_request"));
    assert!(authorization
        .contains("SignedHeaders=content-md5;content-type;host;x-amz-content-sha256;x-amz-date"));
    let signature = authorization
        .split("Signature=")
        .nth(1)
        .expect("signature present");
    assert_eq!(signature.len(), 64);
    assert!(signature.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(captured.header("x-amz-date").is_some());
    assert!(captured.query.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn put_rejects_content_that_does_not_match_the_checksum() {
    let (endpoint, mock) = start_mock().await;
    let store = store(&endpoint);

    let result = run_blocking(move || store.put("acme", HELLO_CHECKSUM, b"tampered")).await;
    assert!(matches!(result, Err(ObjectStoreError::ChecksumMismatch)));
    assert!(mock.requests().is_empty());
}

#[test]
fn presigned_urls_are_not_sent_to_the_server() {
    // The URL is generated locally; this test only checks the key layout and
    // that no HTTP call is involved.
    let store = store("http://127.0.0.1:1");
    let url = store.presigned_url("GET", "acme", HELLO_CHECKSUM);
    assert!(url.starts_with(
        "http://127.0.0.1:1/chef/organization-acme/checksum-5d41402abc4b2a76b9719d911017c592?"
    ));
    assert!(url.contains("X-Amz-Algorithm=AWS4-HMAC-SHA256"));
    assert!(url.contains("X-Amz-Signature="));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn virtual_host_style_requests_target_bucket_subdomain() {
    let (endpoint, mock) = start_mock().await;
    let mut config = S3Config::new(endpoint, "chef", "garage", "AKIAEXAMPLE", "SECRETEXAMPLE");
    config.path_style = false;
    let store = Arc::new(S3ObjectStore::new(config).unwrap());

    // The mock is addressed by 127.0.0.1, so the virtual-host URL cannot be
    // resolved; verify the URL layout instead of the request.
    let url = store.presigned_url("GET", "acme", HELLO_CHECKSUM);
    assert!(url.contains("://chef.127.0.0.1:"));
    assert!(url.contains("/organization-acme/checksum-"));
    assert!(mock.requests().is_empty());
}
