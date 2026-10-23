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

async fn request(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
) -> (StatusCode, serde_json::Value) {
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

#[tokio::test]
async fn environments_crud_and_default_protection() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/environments",
        br#"{"name":"production","description":"prod"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, listed) = request(&app, "GET", "/organizations/acme/environments", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed["production"],
        "http://0.0.0.0:8000/organizations/acme/environments/production"
    );
    assert_eq!(
        listed["_default"],
        "http://0.0.0.0:8000/organizations/acme/environments/_default"
    );

    let (status, updated) = request(
        &app,
        "PUT",
        "/organizations/acme/environments/production",
        br#"{"name":"production","description":"updated"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["description"], "updated");
    assert_eq!(updated["json_class"], "Chef::Environment");

    let (status, _) = request(
        &app,
        "PUT",
        "/organizations/acme/environments/_default",
        br#"{"name":"_default"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);

    let (status, _) = request(
        &app,
        "DELETE",
        "/organizations/acme/environments/production",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn roles_crud() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/roles",
        br#"{"name":"web","run_list":["recipe[apache]"]}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, role) = request(&app, "GET", "/organizations/acme/roles/web", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(role["name"], "web");
    assert_eq!(role["json_class"], "Chef::Role");

    let (status, listed) = request(&app, "GET", "/organizations/acme/roles", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert!(listed["web"].is_string());

    let (status, _) = request(&app, "DELETE", "/organizations/acme/roles/web", b"").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn data_bags_and_items_crud() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/data",
        br#"{"name":"secrets"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = request(
        &app,
        "POST",
        "/organizations/acme/data/secrets",
        br#"{"id":"database","password":"one"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, listed) = request(&app, "GET", "/organizations/acme/data/secrets", b"").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed["database"],
        "http://0.0.0.0:8000/organizations/acme/data/secrets/database"
    );

    let (status, item) = request(
        &app,
        "GET",
        "/organizations/acme/data/secrets/database",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item["password"], "one");

    let (status, item) = request(
        &app,
        "PUT",
        "/organizations/acme/data/secrets/database",
        br#"{"id":"database","password":"two"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(item["password"], "two");

    let (status, _) = request(
        &app,
        "DELETE",
        "/organizations/acme/data/secrets/database",
        b"",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = request(&app, "DELETE", "/organizations/acme/data/secrets", b"").await;
    assert_eq!(status, StatusCode::OK);
}
