mod support;

use axum::http::StatusCode;
use axum::Router;
use microwave_crypto::SignVersion;
use serde_json::Value;
use tower::ServiceExt;

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request(
            method,
            path,
            body,
            SignVersion::V1_3,
            user,
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, support::response_json(response).await)
}

async fn create_org(app: &Router) {
    let (status, _) = send(
        app,
        "POST",
        "/organizations",
        br#"{"name":"acme","full_name":"Acme Corp"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

async fn create_user_with_public_key(app: &Router, username: &str) {
    let body = format!(
        r#"{{"username":"{username}","email":"{username}@example.com","password":"secret","public_key":{}}}"#,
        serde_json::to_string(support::PUBLIC_KEY).unwrap()
    );
    let (status, _) = send(app, "POST", "/users", body.as_bytes(), "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);
}

fn key_names(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|key| key["name"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn manages_a_user_key_lifecycle() {
    let app = support::app(support::state_with_pivotal());
    create_user_with_public_key(&app, "alice").await;

    let (status, list) = send(&app, "GET", "/users/alice/keys", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key_names(&list), vec!["default"]);
    assert_eq!(
        list[0]["uri"],
        "http://0.0.0.0:8000/users/alice/keys/default"
    );
    assert_eq!(list[0]["expired"], false);

    let body = format!(
        r#"{{"name":"alt","public_key":{},"expiration_date":"infinity"}}"#,
        serde_json::to_string(support::PUBLIC_KEY).unwrap()
    );
    let (status, created) = send(
        &app,
        "POST",
        "/users/alice/keys",
        body.as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["uri"], "http://0.0.0.0:8000/users/alice/keys/alt");
    assert_eq!(created["name"], "alt");
    assert_eq!(created["public_key"], support::PUBLIC_KEY.trim());
    assert_eq!(created["expiration_date"], "infinity");
    assert!(created.get("private_key").is_none());

    let (status, list) = send(&app, "GET", "/users/alice/keys", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key_names(&list), vec!["alt", "default"]);

    let (status, key) = send(&app, "GET", "/users/alice/keys/alt", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key["name"], "alt");
    assert_eq!(key["expiration_date"], "infinity");

    let (status, updated) = send(
        &app,
        "PUT",
        "/users/alice/keys/alt",
        br#"{"name":"renamed","expiration_date":"2050-12-24T21:00:00Z"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        updated["uri"],
        "http://0.0.0.0:8000/users/alice/keys/renamed"
    );
    assert_eq!(updated["name"], "renamed");
    assert_eq!(updated["expiration_date"], "2050-12-24T21:00:00Z");

    let (status, _) = send(&app, "GET", "/users/alice/keys/alt", b"", "pivotal").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, key) = send(&app, "GET", "/users/alice/keys/renamed", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key["name"], "renamed");

    let (status, deleted) = send(&app, "DELETE", "/users/alice/keys/renamed", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["name"], "renamed");

    let (status, _) = send(&app, "GET", "/users/alice/keys/renamed", b"", "pivotal").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, list) = send(&app, "GET", "/users/alice/keys", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key_names(&list), vec!["default"]);
}

#[tokio::test]
async fn generates_a_private_key_for_a_new_user_key() {
    let app = support::app(support::state_with_pivotal());
    create_user_with_public_key(&app, "alice").await;

    let (status, created) = send(
        &app,
        "POST",
        "/users/alice/keys",
        br#"{"name":"generated","create_key":true,"expiration_date":"infinity"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["name"], "generated");
    assert_eq!(created["expiration_date"], "infinity");
    assert!(created["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));
    assert!(created["public_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PUBLIC KEY"));

    let (status, key) = send(&app, "GET", "/users/alice/keys/generated", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key["expiration_date"], "infinity");
    assert!(key.get("private_key").is_none());
}

#[tokio::test]
async fn accepts_iso_expiration_dates_and_reports_expiry() {
    let app = support::app(support::state_with_pivotal());
    create_user_with_public_key(&app, "alice").await;

    let body = format!(
        r#"{{"name":"expired","public_key":{},"expiration_date":"2012-12-24T21:00:00Z"}}"#,
        serde_json::to_string(support::PUBLIC_KEY).unwrap()
    );
    let (status, created) = send(
        &app,
        "POST",
        "/users/alice/keys",
        body.as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["expiration_date"], "2012-12-24T21:00:00Z");

    let (status, list) = send(&app, "GET", "/users/alice/keys", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    let expired = list
        .as_array()
        .unwrap()
        .iter()
        .find(|key| key["name"] == "expired")
        .unwrap();
    assert_eq!(expired["expired"], true);

    let (status, _) = send(
        &app,
        "POST",
        "/users/alice/keys",
        br#"{"name":"bad","create_key":true,"expiration_date":"not-a-date"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn manages_a_client_key_lifecycle() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = send(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"web1","create_key":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, list) = send(
        &app,
        "GET",
        "/organizations/acme/clients/web1/keys",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key_names(&list), vec!["default"]);
    assert_eq!(
        list[0]["uri"],
        "http://0.0.0.0:8000/organizations/acme/clients/web1/keys/default"
    );

    let body = format!(
        r#"{{"name":"alt","public_key":{},"expiration_date":"infinity"}}"#,
        serde_json::to_string(support::PUBLIC_KEY).unwrap()
    );
    let (status, created) = send(
        &app,
        "POST",
        "/organizations/acme/clients/web1/keys",
        body.as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/organizations/acme/clients/web1/keys/alt"
    );
    assert_eq!(created["expiration_date"], "infinity");

    let (status, key) = send(
        &app,
        "GET",
        "/organizations/acme/clients/web1/keys/alt",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key["name"], "alt");

    let (status, updated) = send(
        &app,
        "PUT",
        "/organizations/acme/clients/web1/keys/alt",
        br#"{"expiration_date":"2050-12-24T21:00:00Z"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated["name"], "alt");
    assert_eq!(updated["expiration_date"], "2050-12-24T21:00:00Z");

    let (status, deleted) = send(
        &app,
        "DELETE",
        "/organizations/acme/clients/web1/keys/alt",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["name"], "alt");

    let (status, _) = send(
        &app,
        "GET",
        "/organizations/acme/clients/web1/keys/alt",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn generates_a_private_key_for_a_new_client_key() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;

    let (status, _) = send(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"web1","create_key":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, created) = send(
        &app,
        "POST",
        "/organizations/acme/clients/web1/keys",
        br#"{"name":"generated","create_key":true,"expiration_date":"infinity"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(created["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));
    assert!(created["public_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PUBLIC KEY"));
}

#[tokio::test]
async fn reads_org_scoped_user_keys() {
    let app = support::app(support::state_with_pivotal());
    create_org(&app).await;
    create_user_with_public_key(&app, "alice").await;

    let (status, list) = send(
        &app,
        "GET",
        "/organizations/acme/users/alice/keys",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key_names(&list), vec!["default"]);
    assert_eq!(
        list[0]["uri"],
        "http://0.0.0.0:8000/organizations/acme/users/alice/keys/default"
    );

    let (status, key) = send(
        &app,
        "GET",
        "/organizations/acme/users/alice/keys/default",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key["name"], "default");
    assert_eq!(
        key["public_key"].as_str().unwrap().trim(),
        support::PUBLIC_KEY.trim()
    );
}

#[tokio::test]
async fn allows_the_actor_to_manage_its_own_keys() {
    let app = support::app(support::state_with_pivotal());
    create_user_with_public_key(&app, "alice").await;

    let body = format!(
        r#"{{"name":"self","public_key":{},"expiration_date":"infinity"}}"#,
        serde_json::to_string(support::PUBLIC_KEY).unwrap()
    );
    let (status, _) = send(&app, "POST", "/users/alice/keys", body.as_bytes(), "alice").await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = send(&app, "DELETE", "/users/alice/keys/self", b"", "alice").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn forbids_other_actors_from_managing_keys() {
    let app = support::app(support::state_with_pivotal());
    create_user_with_public_key(&app, "alice").await;
    create_user_with_public_key(&app, "bob").await;

    let (status, _) = send(&app, "GET", "/users/alice/keys", b"", "bob").await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = send(&app, "DELETE", "/users/alice/keys/default", b"", "bob").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn returns_not_found_for_missing_actors_and_keys() {
    let app = support::app(support::state_with_pivotal());
    create_user_with_public_key(&app, "alice").await;

    let (status, _) = send(&app, "GET", "/users/nobody/keys", b"", "pivotal").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(&app, "GET", "/users/alice/keys/missing", b"", "pivotal").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rejects_duplicate_and_invalid_key_payloads() {
    let app = support::app(support::state_with_pivotal());
    create_user_with_public_key(&app, "alice").await;

    let (status, _) = send(
        &app,
        "POST",
        "/users/alice/keys",
        br#"{"name":"default","create_key":true,"expiration_date":"infinity"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _) = send(
        &app,
        "POST",
        "/users/alice/keys",
        br#"{"name":"bad name","create_key":true,"expiration_date":"infinity"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = send(
        &app,
        "POST",
        "/users/alice/keys",
        br#"{"name":"both","create_key":true,"public_key":"key","expiration_date":"infinity"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = send(
        &app,
        "POST",
        "/users/alice/keys",
        br#"{"name":"nokey","expiration_date":"infinity"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = send(
        &app,
        "PUT",
        "/users/alice/keys/default",
        br#"{"name":"renamed"}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = send(
        &app,
        "PUT",
        "/users/alice/keys/default",
        br#"{}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
