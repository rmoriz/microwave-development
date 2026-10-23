mod support;

use axum::http::StatusCode;
use microwave_crypto::SignVersion;
use tower::ServiceExt;

#[tokio::test]
async fn creates_an_organization_and_lists_it() {
    let app = support::app(support::state_with_pivotal());
    let body = br#"{"name":"acme","full_name":"Acme Corp"}"#;
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations",
            body,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = support::response_json(response).await;
    assert_eq!(created["uri"], "http://0.0.0.0:8000/organizations/acme");

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed = support::response_json(response).await;
    assert_eq!(listed["acme"], "http://0.0.0.0:8000/organizations/acme");

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let org = support::response_json(response).await;
    assert_eq!(org["name"], "acme");
    assert_eq!(org["full_name"], "Acme Corp");

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "DELETE",
            "/organizations/acme",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
