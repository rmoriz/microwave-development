mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use microwave_crypto::SignVersion;
use tower::ServiceExt;

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

#[tokio::test]
async fn sandbox_upload_and_commit() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let checksum = "5d41402abc4b2a76b9719d911017c592";
    let body = format!(r#"{{"checksums":{{"{checksum}":null}}}}"#);
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/sandboxes",
            body.as_bytes(),
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = support::response_json(response).await;
    assert_eq!(created["checksums"][checksum]["needs_upload"], true);
    let url = created["checksums"][checksum]["url"].as_str().unwrap();
    let path = url.strip_prefix("http://0.0.0.0:8000").unwrap();

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(path)
                .header("content-md5", "XrY7u+Ae7tCTyyK7j1rNww==")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let sandbox_id = created["sandbox_id"].as_str().unwrap();
    let commit_path = format!("/organizations/acme/sandboxes/{sandbox_id}");
    let response = app
        .oneshot(support::signed_request(
            "PUT",
            &commit_path,
            br#"{"is_completed":true}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
