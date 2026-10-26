//! Contract tests for the local bookshelf/object-store HTTP surface.
//!
//! Upstream bookshelf returns `202` on upload, `406` on checksum mismatch and
//! `204` on delete, and GET/HEAD carry `ETag`, `Last-Modified` and
//! `Cache-Control`.

mod support;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use microwave_api::bookshelf::{CACHE_CONTROL_MAX_AGE, MAX_UPLOAD_BYTES};
use microwave_objectstore::{bookshelf_token, checksum_md5_base64, md5_hex};
use tower::ServiceExt;

const ORG: &str = "bookshelf-contract";
const SECRET: &str = "test-secret";

fn object_path(checksum: &str, method: &str) -> String {
    let token = bookshelf_token(SECRET, method, ORG, checksum);
    format!("/bookshelf/organizations/{ORG}/checksums/{checksum}?token={token}")
}

async fn send(app: &Router, method: &str, path: &str, body: &[u8]) -> axum::response::Response {
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

async fn body_bytes(response: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

fn header_str(response: &axum::response::Response, name: axum::http::HeaderName) -> &str {
    response
        .headers()
        .get(&name)
        .unwrap_or_else(|| panic!("missing {name} header"))
        .to_str()
        .unwrap()
}

#[tokio::test]
async fn put_get_head_delete_roundtrip_carries_upstream_statuses_and_headers() {
    let app = support::app(support::state_with_pivotal());
    let contents = b"hello cookbook file";
    let checksum = md5_hex(contents);
    let expected_etag = format!("\"{}\"", checksum_md5_base64(&checksum).unwrap());

    let response = send(&app, "PUT", &object_path(&checksum, "PUT"), contents).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(header_str(&response, header::ETAG), expected_etag);

    let response = send(&app, "GET", &object_path(&checksum, "GET"), b"").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_str(&response, header::CONTENT_TYPE),
        "application/octet-stream"
    );
    assert_eq!(header_str(&response, header::ETAG), expected_etag);
    assert_eq!(
        header_str(&response, header::CACHE_CONTROL),
        format!("max-age={CACHE_CONTROL_MAX_AGE}")
    );
    let last_modified = header_str(&response, header::LAST_MODIFIED).to_string();
    assert!(
        last_modified.ends_with(" GMT"),
        "Last-Modified is not an HTTP date: {last_modified}"
    );
    assert_eq!(body_bytes(response).await, contents);

    let response = send(&app, "HEAD", &object_path(&checksum, "GET"), b"").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_str(&response, header::ETAG), expected_etag);
    assert_eq!(header_str(&response, header::LAST_MODIFIED), last_modified);
    assert_eq!(
        header_str(&response, header::CACHE_CONTROL),
        format!("max-age={CACHE_CONTROL_MAX_AGE}")
    );
    assert!(
        body_bytes(response).await.is_empty(),
        "HEAD returned a body"
    );

    let response = send(&app, "DELETE", &object_path(&checksum, "DELETE"), b"").await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        body_bytes(response).await.is_empty(),
        "DELETE returned a body"
    );

    let response = send(&app, "GET", &object_path(&checksum, "GET"), b"").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn checksum_mismatch_returns_406_and_stores_nothing() {
    let app = support::app(support::state_with_pivotal());
    let contents = b"contents that do not match the URL checksum";
    let claimed_checksum = md5_hex(b"a different payload");

    let response = send(
        &app,
        "PUT",
        &object_path(&claimed_checksum, "PUT"),
        contents,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);

    let response = send(&app, "GET", &object_path(&claimed_checksum, "GET"), b"").await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "a rejected upload must not create an object"
    );
}

#[tokio::test]
async fn missing_object_returns_404_for_get_and_head() {
    let app = support::app(support::state_with_pivotal());
    let missing = md5_hex(b"never uploaded");

    let response = send(&app, "GET", &object_path(&missing, "GET"), b"").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = send(&app, "HEAD", &object_path(&missing, "GET"), b"").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(
        body_bytes(response).await.is_empty(),
        "HEAD 404 returned a body"
    );
}

#[tokio::test]
async fn the_token_still_guards_every_method() {
    let app = support::app(support::state_with_pivotal());
    let contents = b"guarded";
    let checksum = md5_hex(contents);
    let path = format!("/bookshelf/organizations/{ORG}/checksums/{checksum}?token=bogus");

    for method in ["PUT", "GET", "HEAD", "DELETE"] {
        let response = send(&app, method, &path, contents).await;
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} accepted a bogus token"
        );
    }
}

#[test]
fn upload_limit_matches_upstream_250_mib() {
    // Equality to upstream nginx `client_max_body_size 250m` also proves the
    // old 64 MiB cap is gone.
    assert_eq!(MAX_UPLOAD_BYTES, 250 * 1024 * 1024);
}

#[tokio::test]
async fn bodies_over_the_configured_limit_are_rejected_before_the_handler() {
    // Exercise the same layer with a small cap so the test stays cheap.
    let app =
        microwave_api::bookshelf::routes_with_limit(1024).with_state(support::state_with_pivotal());
    let contents = vec![b'x'; 2048];
    let checksum = md5_hex(&contents);

    let response = send(&app, "PUT", &object_path(&checksum, "PUT"), &contents).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
