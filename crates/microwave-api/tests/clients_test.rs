mod support;

use axum::http::StatusCode;
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
async fn creates_a_client_and_reads_principals() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/clients",
            br#"{"name":"web1","validator":false,"create_key":true}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = support::response_json(response).await;
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/organizations/acme/clients/web1"
    );
    assert!(created["chef_key"]["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/clients",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed = support::response_json(response).await;
    assert_eq!(
        listed["web1"],
        "http://0.0.0.0:8000/organizations/acme/clients/web1"
    );

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/clients/web1",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let client = support::response_json(response).await;
    assert_eq!(client["name"], "web1");
    assert_eq!(client["validator"], false);

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/principals/web1",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let principals = support::response_json(response).await;
    assert_eq!(principals["principals"][0]["name"], "web1");
    assert_eq!(principals["principals"][0]["type"], "client");

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "DELETE",
            "/organizations/acme/clients/web1",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
