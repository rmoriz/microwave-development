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
async fn uploads_and_reads_a_cookbook_version() {
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
    let url = created["checksums"][checksum]["url"].as_str().unwrap();
    let path = url.strip_prefix("http://0.0.0.0:8000").unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(path)
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let sandbox_id = created["sandbox_id"].as_str().unwrap();
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "PUT",
            &format!("/organizations/acme/sandboxes/{sandbox_id}"),
            br#"{"is_completed":true}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let manifest = format!(
        r#"{{"name":"apache","version":"1.0.0","metadata":{{"version":"1.0.0"}},"recipes":[{{"name":"default.rb","path":"recipes/default.rb","checksum":"{checksum}","specificity":"default"}}]}}"#
    );
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "PUT",
            "/organizations/acme/cookbooks/apache/1.0.0",
            manifest.as_bytes(),
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/cookbooks",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed = support::response_json(response).await;
    assert_eq!(listed["apache"]["versions"][0]["version"], "1.0.0");

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/cookbooks/apache/1.0.0",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = support::response_json(response).await;
    let url = fetched["recipes"][0]["url"].as_str().unwrap();
    assert!(url.contains("/bookshelf/organizations/"));
    assert!(url.contains("/checksums/"));
    let path = url.strip_prefix("http://0.0.0.0:8000").unwrap();
    let response = app
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&bytes[..], b"hello");
}
