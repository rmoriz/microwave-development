//! An organization without an object store bucket must not look like a missing
//! blob. If `HEAD` answered `404`, a chef client would keep re-uploading into
//! nowhere; an unmapped organization is a server-side failure, so `HEAD` and
//! `GET` answer `500`. A genuinely absent blob stays a `404`.
//!
//! Both doubles return `false` from `exists` so the legacy existence fallback
//! cannot mask the metadata error.

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use microwave_api::AppState;
use microwave_objectstore::{
    bookshelf_token, md5_hex, ObjectMetadata, ObjectStore, ObjectStoreError,
};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tower::ServiceExt;

const ORG: &str = "no-bucket-org";

fn expiry() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 900
}

fn object_path(secret: &str, checksum: &str, method: &str) -> String {
    let expires_at = expiry();
    let token = bookshelf_token(secret, method, ORG, checksum, expires_at);
    format!("/bookshelf/organizations/{ORG}/checksums/{checksum}?token={token}&exp={expires_at}")
}

async fn send(app: &Router, method: &str, path: &str, body: &[u8]) -> Response {
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

/// Upload a blob through the working local store so the checksum is real and
/// the working state is proven before the store is swapped out.
async fn upload_fixture() -> (AppState, String) {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    let contents = b"blob that lives in the wrong bucket";
    let checksum = md5_hex(contents);
    let secret = state.config.object_store.secret.clone().unwrap();

    let response = send(
        &app,
        "PUT",
        &object_path(&secret, &checksum, "PUT"),
        contents,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(&app, "HEAD", &object_path(&secret, &checksum, "GET"), b"").await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the working store must serve the uploaded blob"
    );

    (state, checksum)
}

/// Rebuild the app around the same store and config but a different object
/// store, mirroring the real wiring.
fn app_with(state: &AppState, store: impl ObjectStore + 'static) -> Router {
    support::app(
        AppState::new(
            Arc::clone(&state.store),
            state.config.clone(),
            Arc::new(store),
        )
        .unwrap(),
    )
}

struct NoBucketStore;

impl ObjectStore for NoBucketStore {
    fn put(
        &self,
        _organization_id: &str,
        _checksum: &str,
        _data: &[u8],
    ) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::NoOrgBucket)
    }

    fn get(&self, _organization_id: &str, _checksum: &str) -> Result<Vec<u8>, ObjectStoreError> {
        Err(ObjectStoreError::NoOrgBucket)
    }

    fn exists(&self, _organization_id: &str, _checksum: &str) -> bool {
        false
    }

    fn delete(&self, _organization_id: &str, _checksum: &str) -> Result<bool, ObjectStoreError> {
        Err(ObjectStoreError::NoOrgBucket)
    }

    fn metadata(
        &self,
        _organization_id: &str,
        _checksum: &str,
    ) -> Result<Option<ObjectMetadata>, ObjectStoreError> {
        Err(ObjectStoreError::NoOrgBucket)
    }

    fn presigned_url(
        &self,
        _method: &str,
        _organization_id: &str,
        _checksum: &str,
    ) -> Result<String, ObjectStoreError> {
        Err(ObjectStoreError::NoOrgBucket)
    }
}

struct NotFoundStore;

impl ObjectStore for NotFoundStore {
    fn put(
        &self,
        _organization_id: &str,
        _checksum: &str,
        _data: &[u8],
    ) -> Result<(), ObjectStoreError> {
        unreachable!()
    }

    fn get(&self, _organization_id: &str, _checksum: &str) -> Result<Vec<u8>, ObjectStoreError> {
        Err(ObjectStoreError::NotFound)
    }

    fn exists(&self, _organization_id: &str, _checksum: &str) -> bool {
        false
    }

    fn delete(&self, _organization_id: &str, _checksum: &str) -> Result<bool, ObjectStoreError> {
        Err(ObjectStoreError::NotFound)
    }

    fn metadata(
        &self,
        _organization_id: &str,
        _checksum: &str,
    ) -> Result<Option<ObjectMetadata>, ObjectStoreError> {
        Err(ObjectStoreError::NotFound)
    }

    fn presigned_url(
        &self,
        _method: &str,
        _organization_id: &str,
        _checksum: &str,
    ) -> Result<String, ObjectStoreError> {
        unreachable!()
    }
}

#[tokio::test]
async fn head_and_get_answer_500_for_an_organization_without_a_bucket() {
    let (state, checksum) = upload_fixture().await;
    let app = app_with(&state, NoBucketStore);
    let secret = state.config.object_store.secret.clone().unwrap();
    let path = object_path(&secret, &checksum, "GET");

    let response = send(&app, "HEAD", &path, b"").await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let response = send(&app, "GET", &path, b"").await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn head_and_get_answer_404_for_a_missing_blob() {
    let (state, checksum) = upload_fixture().await;
    let app = app_with(&state, NotFoundStore);
    let secret = state.config.object_store.secret.clone().unwrap();
    let path = object_path(&secret, &checksum, "GET");

    let response = send(&app, "HEAD", &path, b"").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = send(&app, "GET", &path, b"").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
