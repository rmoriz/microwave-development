mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_crypto::SignVersion;
use serde_json::{json, Value};
use tower::ServiceExt;

const HELLO_CHECKSUM: &str = "5d41402abc4b2a76b9719d911017c592";
const WORLD_CHECKSUM: &str = "7d793037a0760186574b0282f2f435e7";

async fn create_org(app: &axum::Router) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations",
            br#"{"name":"acme","full_name":"Acme Corp"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

async fn request(app: &axum::Router, method: &str, path: &str, body: &[u8]) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            method,
            path,
            body,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    let status = response.status();
    let value = support::response_json(response).await;
    (status, value)
}

async fn create_sandbox(app: &axum::Router, checksums: &[&str]) -> (StatusCode, Value) {
    let body = serde_json::to_vec(&json!({
        "checksums": checksums
            .iter()
            .map(|checksum| (checksum.to_string(), Value::Null))
            .collect::<serde_json::Map<_, _>>()
    }))
    .unwrap();
    request(app, "POST", "/organizations/acme/sandboxes", &body).await
}

async fn upload(app: &axum::Router, url: &str, content: &str) {
    let path = url.strip_prefix("http://0.0.0.0:8000").unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(path)
                .header("content-md5", "XrY7u+Ae7tCTyyK7j1rNww==")
                .body(Body::from(content.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn sandbox_upload_and_commit() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, created) = create_sandbox(&app, &[HELLO_CHECKSUM]).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["checksums"][HELLO_CHECKSUM]["needs_upload"], true);
    let url = created["checksums"][HELLO_CHECKSUM]["url"]
        .as_str()
        .unwrap();
    upload(&app, url, "hello").await;

    let sandbox_id = created["sandbox_id"].as_str().unwrap();
    let commit_path = format!("/organizations/acme/sandboxes/{sandbox_id}");
    let (status, _) = request(&app, "PUT", &commit_path, br#"{"is_completed":true}"#).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn validates_sandbox_checksums() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, error) = request(&app, "POST", "/organizations/acme/sandboxes", br#"{}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error, json!({"error": ["Field 'checksums' missing"]}));

    let (status, error) = request(
        &app,
        "POST",
        "/organizations/acme/sandboxes",
        br#"{"checksums":{}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error, json!({"error": ["Bad checksums!"]}));

    let body = format!(r#"{{"checksums":{{"{HELLO_CHECKSUM}":"foo"}}}}"#);
    let (status, error) = request(
        &app,
        "POST",
        "/organizations/acme/sandboxes",
        body.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error, json!({"error": ["Bad checksums!"]}));

    let (status, error) = request(
        &app,
        "POST",
        "/organizations/acme/sandboxes",
        br#"{"checksums":{"not-a-checksum":null}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error, json!({"error": ["Bad checksums!"]}));
}

#[tokio::test]
async fn recognizes_checksums_that_are_already_uploaded() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, first) = create_sandbox(&app, &[HELLO_CHECKSUM]).await;
    assert_eq!(status, StatusCode::CREATED);
    let url = first["checksums"][HELLO_CHECKSUM]["url"].as_str().unwrap();
    upload(&app, url, "hello").await;
    let (status, _) = request(
        &app,
        "PUT",
        &format!(
            "/organizations/acme/sandboxes/{}",
            first["sandbox_id"].as_str().unwrap()
        ),
        br#"{"is_completed":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, second) = create_sandbox(&app, &[HELLO_CHECKSUM, WORLD_CHECKSUM]).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        second["checksums"][HELLO_CHECKSUM],
        json!({"needs_upload": false})
    );
    assert_eq!(
        second["checksums"][WORLD_CHECKSUM]["needs_upload"],
        json!(true)
    );
    assert!(second["checksums"][WORLD_CHECKSUM]["url"].is_string());
}

#[tokio::test]
async fn commit_returns_the_upstream_sandbox_document() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, created) = create_sandbox(&app, &[HELLO_CHECKSUM, WORLD_CHECKSUM]).await;
    assert_eq!(status, StatusCode::CREATED);
    for (checksum, content) in [(HELLO_CHECKSUM, "hello"), (WORLD_CHECKSUM, "world")] {
        let url = created["checksums"][checksum]["url"].as_str().unwrap();
        upload(&app, url, content).await;
    }

    let sandbox_id = created["sandbox_id"].as_str().unwrap();
    let (status, committed) = request(
        &app,
        "PUT",
        &format!("/organizations/acme/sandboxes/{sandbox_id}"),
        br#"{"is_completed":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(committed["guid"], sandbox_id);
    assert_eq!(committed["name"], sandbox_id);
    assert_eq!(committed["is_completed"], true);
    let checksums = committed["checksums"].as_array().unwrap();
    assert_eq!(checksums.len(), 2);
    assert!(checksums.iter().any(|value| value == HELLO_CHECKSUM));
    assert!(checksums.iter().any(|value| value == WORLD_CHECKSUM));

    let create_time = committed["create_time"].as_str().unwrap();
    assert_eq!(create_time.len(), 25, "create_time {create_time}");
    assert!(create_time.contains('T'));
    assert!(
        create_time.ends_with("+00:00"),
        "create_time {create_time} should use the upstream offset form"
    );
}

#[tokio::test]
async fn committing_an_incomplete_sandbox_returns_503() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, created) = create_sandbox(&app, &[HELLO_CHECKSUM, WORLD_CHECKSUM]).await;
    assert_eq!(status, StatusCode::CREATED);
    let sandbox_id = created["sandbox_id"].as_str().unwrap();
    let (status, error) = request(
        &app,
        "PUT",
        &format!("/organizations/acme/sandboxes/{sandbox_id}"),
        br#"{"is_completed":true}"#,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let mut sums = [HELLO_CHECKSUM, WORLD_CHECKSUM];
    sums.sort_unstable();
    assert_eq!(
        error,
        json!({
            "error": [format!(
                "Cannot update sandbox {sandbox_id}: the following checksums have not been uploaded: {}",
                sums.join(", ")
            )]
        })
    );
}
