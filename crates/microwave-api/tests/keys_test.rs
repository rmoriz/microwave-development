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

async fn send_v0(
    app: &Router,
    method: &str,
    path: &str,
    body: &[u8],
    user: &str,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request_with_api_version(
            method,
            path,
            body,
            SignVersion::V1_3,
            user,
            "0",
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
        r#"{{"username":"{username}","email":"{username}@example.com","display_name":"{username}","password":"secret","public_key":{}}}"#,
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
    assert_eq!(
        created,
        serde_json::json!({ "uri": "http://0.0.0.0:8000/users/alice/keys/alt" })
    );

    let (status, list) = send(&app, "GET", "/users/alice/keys", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key_names(&list), vec!["alt", "default"]);

    let (status, key) = send(&app, "GET", "/users/alice/keys/alt", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key["name"], "alt");
    assert_eq!(key["expiration_date"], "infinity");
    assert_eq!(key["public_key"], support::PUBLIC_KEY);

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
        updated,
        serde_json::json!({
            "name": "renamed",
            "expiration_date": "2050-12-24T21:00:00Z",
        })
    );

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
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/users/alice/keys/generated"
    );
    assert!(created.get("name").is_none());
    assert!(created.get("public_key").is_none());
    assert!(created["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));

    let (status, key) = send(&app, "GET", "/users/alice/keys/generated", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key["expiration_date"], "infinity");
    assert!(key["public_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PUBLIC KEY"));
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
    let (status, _) = send(
        &app,
        "POST",
        "/users/alice/keys",
        body.as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, created_key) =
        send(&app, "GET", "/users/alice/keys/expired", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created_key["expiration_date"], "2012-12-24T21:00:00Z");

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
        created,
        serde_json::json!({
            "uri": "http://0.0.0.0:8000/organizations/acme/clients/web1/keys/alt",
        })
    );

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
    assert_eq!(
        updated,
        serde_json::json!({ "expiration_date": "2050-12-24T21:00:00Z" })
    );

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
    assert_eq!(
        created["uri"],
        "http://0.0.0.0:8000/organizations/acme/clients/web1/keys/generated"
    );
    assert!(created["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));
    assert!(created.get("public_key").is_none());

    let (status, key) = send(
        &app,
        "GET",
        "/organizations/acme/clients/web1/keys/generated",
        b"",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(key["public_key"]
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
    assert_eq!(key["public_key"], support::PUBLIC_KEY);
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
async fn webui_can_impersonate_an_org_member() {
    use tower::ServiceExt;

    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    let org = support::add_org(&state, "acme").await;
    let bob = state
        .sqlite()
        .create_user("bob", "bob@example.com", "bcrypt", "h", "s", false)
        .unwrap();
    support::associate(&state, &org, &bob);

    let mut request = support::signed_request(
        "GET",
        "/organizations/acme/users/bob",
        b"",
        SignVersion::V1_3,
        "bob",
    );
    request
        .headers_mut()
        .insert("x-ops-request-source", "web".parse().unwrap());
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Without the web source marker the superuser key does not authenticate
    // the request as bob.
    let request = support::signed_request(
        "GET",
        "/organizations/acme/users/bob",
        b"",
        SignVersion::V1_3,
        "bob",
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn forbids_deleting_or_updating_the_authenticating_key() {
    let app = support::app(support::state_with_pivotal());
    create_user_with_public_key(&app, "alice").await;

    let (status, body) = send(&app, "DELETE", "/users/alice/keys/default", b"", "alice").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        serde_json::json!({
            "error": "The key 'default' was used to authenticate this request and cannot be modified or deleted."
        })
    );

    let (status, _) = send(
        &app,
        "PUT",
        "/users/alice/keys/default",
        br#"{"expiration_date":"2050-12-24T21:00:00Z"}"#,
        "alice",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn expired_keys_cannot_authenticate() {
    let app = support::app(support::state_with_pivotal());
    create_user_with_public_key(&app, "alice").await;

    let (status, _) = send(&app, "DELETE", "/users/alice/keys/default", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);

    let body = format!(
        r#"{{"name":"default","public_key":{},"expiration_date":"2012-12-24T21:00:00Z"}}"#,
        serde_json::to_string(support::PUBLIC_KEY).unwrap()
    );
    let (status, _) = send(
        &app,
        "POST",
        "/users/alice/keys",
        body.as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = send(&app, "GET", "/users/alice", b"", "alice").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn replacing_the_default_key_resets_its_expiration() {
    let app = support::app(support::state_with_pivotal());
    create_user_with_public_key(&app, "alice").await;

    let (status, _) = send(&app, "DELETE", "/users/alice/keys/default", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    let body = format!(
        r#"{{"name":"default","public_key":{},"expiration_date":"2012-12-24T21:00:00Z"}}"#,
        serde_json::to_string(support::PUBLIC_KEY).unwrap()
    );
    let (status, _) = send(
        &app,
        "POST",
        "/users/alice/keys",
        body.as_bytes(),
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let body = format!(
        r#"{{"username":"alice","display_name":"alice","email":"alice@example.com","public_key":{}}}"#,
        serde_json::to_string(support::PUBLIC_KEY).unwrap()
    );
    let (status, _) = send_v0(&app, "PUT", "/users/alice", body.as_bytes(), "pivotal").await;
    assert_eq!(status, StatusCode::OK);

    let (status, key) = send(&app, "GET", "/users/alice/keys/default", b"", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(key["expiration_date"], "infinity");
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

#[tokio::test]
async fn org_scoped_key_reads_follow_public_key_read_access_membership() {
    let state = support::state_with_pivotal();
    let app = support::app(state.clone());
    create_org(&app).await;
    create_user_with_public_key(&app, "alice").await;
    create_user_with_public_key(&app, "bob").await;

    let org = state.sqlite().find_org("acme").unwrap().unwrap();
    let bob = state.sqlite().find_user("bob").unwrap().unwrap();
    state.sqlite().add_user_to_org(&org.id, &bob.id).unwrap();

    let (status, _) = send(
        &app,
        "POST",
        "/organizations/acme/clients",
        br#"{"name":"web1","create_key":true}"#,
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = send(
        &app,
        "GET",
        "/organizations/acme/clients/web1/keys",
        b"",
        "bob",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = send(
        &app,
        "GET",
        "/organizations/acme/users/alice/keys",
        b"",
        "bob",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let public_key_read = state
        .sqlite()
        .find_group(&org.id, "public_key_read_access")
        .unwrap()
        .unwrap();
    state
        .sqlite()
        .replace_group_members(&org.id, &public_key_read.authz_id, None, None, Some(&[]))
        .unwrap();

    let (status, _) = send(
        &app,
        "GET",
        "/organizations/acme/clients/web1/keys",
        b"",
        "bob",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(
        &app,
        "GET",
        "/organizations/acme/users/alice/keys",
        b"",
        "bob",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    state
        .sqlite()
        .replace_group_members(
            &org.id,
            &public_key_read.authz_id,
            None,
            None,
            Some(&["users".to_string(), "clients".to_string()]),
        )
        .unwrap();
    let (status, _) = send(
        &app,
        "GET",
        "/organizations/acme/clients/web1/keys",
        b"",
        "bob",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
