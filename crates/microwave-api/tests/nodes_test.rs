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
async fn creates_lists_reads_updates_and_deletes_a_node() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let node = br#"{"name":"web1","run_list":["recipe[apache]"],"normal":{"role":"web"}}"#;
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/organizations/acme/nodes",
            node,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = support::response_json(response).await;
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/organizations/acme/nodes/web1"
    );

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/nodes",
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
        "http://0.0.0.0:8000/organizations/acme/nodes/web1"
    );

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/nodes/web1",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = support::response_json(response).await;
    assert_eq!(fetched["name"], "web1");
    assert_eq!(fetched["chef_environment"], "_default");
    assert_eq!(fetched["json_class"], "Chef::Node");

    let updated = br#"{"name":"web1","chef_environment":"production","run_list":[]}"#;
    let response = app
        .clone()
        .oneshot(support::signed_request(
            "PUT",
            "/organizations/acme/nodes/web1",
            updated,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = support::response_json(response).await;
    assert_eq!(fetched["chef_environment"], "production");

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "DELETE",
            "/organizations/acme/nodes/web1",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(support::signed_request(
            "GET",
            "/organizations/acme/nodes/web1",
            b"",
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
