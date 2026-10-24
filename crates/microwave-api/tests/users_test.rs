mod support;

use axum::http::{header, StatusCode};
use microwave_crypto::{generate_rsa_keypair, SignVersion};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn get(app: &axum::Router, path: &str, user: &str) -> (StatusCode, Value) {
    get_with_version(app, path, user, "2").await
}

async fn get_with_version(
    app: &axum::Router,
    path: &str,
    user: &str,
    api_version: &str,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request_with_api_version(
            "GET",
            path,
            b"",
            SignVersion::V1_3,
            user,
            api_version,
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, support::response_json(response).await)
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: &[u8],
    api_version: &str,
    user: &str,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let response = app
        .clone()
        .oneshot(support::signed_request_with_api_version(
            method,
            path,
            body,
            SignVersion::V1_3,
            user,
            api_version,
        ))
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    (status, headers, support::response_json(response).await)
}

#[tokio::test]
async fn creates_lists_and_reads_a_user() {
    let app = support::app(support::state_with_pivotal());
    let body = br#"{"username":"alice","email":"alice@example.com","first_name":"Alice","last_name":"Liddell","display_name":"Alice L.","password":"secret","create_key":true}"#;
    let (status, _, created) = send(&app, "POST", "/users", body, "2", "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["uri"], "http://0.0.0.0:8000/users/alice");
    assert!(created["chef_key"]["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));

    let (status, listed) = get(&app, "/users", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["alice"], "http://0.0.0.0:8000/users/alice");

    let (status, user) = get(&app, "/users/alice", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(user["username"], "alice");
    assert_eq!(user["email"], "alice@example.com");
    assert_eq!(user["first_name"], "Alice");
    assert_eq!(user["last_name"], "Liddell");
    assert_eq!(user["display_name"], "Alice L.");
    assert!(user.get("public_key").is_none(), "v1+ omits public_key");
    assert!(
        user.get("admin").is_none(),
        "admin is not part of the user body"
    );
}

#[tokio::test]
async fn v1_create_without_key_fields_creates_no_default_key() {
    let app = support::app(support::state_with_pivotal());
    let body = br#"{"username":"alice","email":"alice@example.com","display_name":"Alice","password":"secret"}"#;
    let (status, _, created) = send(&app, "POST", "/users", body, "2", "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created, json!({"uri": "http://0.0.0.0:8000/users/alice"}));

    let (status, _) = get(&app, "/users/alice/keys/default", "pivotal").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn v0_create_generates_a_default_key_and_returns_the_private_key() {
    let app = support::app(support::state_with_pivotal());
    let body = br#"{"username":"alice","email":"alice@example.com","display_name":"Alice","password":"secret"}"#;
    let (status, _, created) = send(&app, "POST", "/users", body, "0", "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["uri"], "http://0.0.0.0:8000/users/alice");
    assert!(created["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));
    assert!(created.get("chef_key").is_none());

    let (status, user) = get_with_version(&app, "/users/alice", "pivotal", "0").await;
    assert_eq!(status, StatusCode::OK);
    assert!(user["public_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PUBLIC KEY"));
}

#[tokio::test]
async fn create_validates_username_display_name_email_and_password() {
    let app = support::app(support::state_with_pivotal());
    let cases = [
        br#"{"username":"Alice","email":"alice@example.com","display_name":"Alice","password":"secret"}"#.as_slice(),
        br#"{"username":"alice smith","email":"alice@example.com","display_name":"Alice","password":"secret"}"#.as_slice(),
        br#"{"username":"alice","email":"alice@example.com","password":"secret"}"#.as_slice(),
        br#"{"username":"alice","display_name":"Alice","password":"secret"}"#.as_slice(),
        br#"{"username":"alice","email":"alice@foo @ bar","display_name":"Alice","password":"secret"}"#.as_slice(),
        br#"{"username":"alice","email":"alice@example.com","display_name":"Alice"}"#.as_slice(),
    ];
    for body in cases {
        let (status, _, _) = send(&app, "POST", "/users", body, "2", "pivotal").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "body: {}",
            String::from_utf8_lossy(body)
        );
    }
}

#[tokio::test]
async fn create_allows_external_auth_without_password_or_email() {
    let app = support::app(support::state_with_pivotal());
    let body = br#"{"username":"saml","display_name":"SAML User","external_authentication_uid":"sam.l.jackson"}"#;
    let (status, _, _) = send(&app, "POST", "/users", body, "2", "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, user) = get(&app, "/users/saml", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(user["email"], "");
}

#[tokio::test]
async fn create_accepts_spaces_and_utf8_in_names_and_ignores_bogus_fields() {
    let app = support::app(support::state_with_pivotal());
    let body = r#"{"username":"alice","email":"alice@example.com","first_name":"Guðrún","middle_name":"","last_name":"van Dijk","display_name":"超人","password":"secret","bogus":"ignored"}"#;
    let (status, _, _) = send(&app, "POST", "/users", body.as_bytes(), "2", "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);

    let (_, user) = get(&app, "/users/alice", "pivotal").await;
    assert_eq!(user["first_name"], "Guðrún");
    assert_eq!(user["middle_name"], "");
    assert_eq!(user["last_name"], "van Dijk");
    assert_eq!(user["display_name"], "超人");
}

#[tokio::test]
async fn duplicate_user_conflicts() {
    let app = support::app(support::state_with_pivotal());
    let body = br#"{"username":"alice","email":"alice@example.com","display_name":"Alice","password":"secret"}"#;
    for expected in [StatusCode::CREATED, StatusCode::CONFLICT] {
        let (status, _, _) = send(&app, "POST", "/users", body, "2", "pivotal").await;
        assert_eq!(status, expected);
    }
}

#[tokio::test]
async fn create_conflicts_on_case_insensitive_email() {
    let app = support::app(support::state_with_pivotal());
    let first = br#"{"username":"alice","email":"User@aol.com","display_name":"Alice","password":"secret"}"#;
    let (status, _, _) = send(&app, "POST", "/users", first, "2", "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);

    let second =
        br#"{"username":"bob","email":"user@AOL.COM","display_name":"Bob","password":"secret"}"#;
    let (status, _, body) = send(&app, "POST", "/users", second, "2", "pivotal").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        body,
        json!({"error": ["Username or email address already in use."]})
    );
}

#[tokio::test]
async fn list_users_filters_by_email_case_insensitively_and_external_uid() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "alice", "User@aol.com", false);
    let saml = support::add_user(&state, "saml", "saml@example.com", false);
    state
        .store
        .update_user_patch(
            "saml",
            &microwave_store::UserPatch {
                external_authentication_uid: Some(Some("sam.l.jackson".into())),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(saml.username, "saml");
    let app = support::app(state);

    for email in ["User@aol.com", "USER@AOL.COM", "user@aol.com"] {
        let path = format!("/users?email={email}");
        let (status, listed) = get(&app, &path, "pivotal").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(listed, json!({"alice": "http://0.0.0.0:8000/users/alice"}));
    }

    let (status, listed) = get(
        &app,
        "/users?external_authentication_uid=sam.l.jackson",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed, json!({"saml": "http://0.0.0.0:8000/users/saml"}));

    let (status, listed) = get(&app, "/users?external_authentication_uid=nobody", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed, json!({}));
}

#[tokio::test]
async fn list_users_verbose_returns_profile_objects() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let (status, listed) = get(&app, "/users?verbose=true", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    for name in ["pivotal", "alice"] {
        let data = &listed[name];
        assert_eq!(data["first_name"], name);
        assert_eq!(data["last_name"], name);
        assert_eq!(data["display_name"], name);
        assert!(data.get("email").is_some());
    }
}

#[tokio::test]
async fn list_users_requires_the_platform_superuser() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "admin", "admin@example.com", true);
    support::add_user(&state, "normal", "normal@example.com", false);
    let app = support::app(state);

    for user in ["admin", "normal"] {
        let (status, _) = get(&app, "/users", user).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "user {user}");
    }
}

#[tokio::test]
async fn get_user_allows_self_and_users_sharing_an_org() {
    let state = support::state_with_pivotal();
    let org = support::add_org(&state, "acme");
    let target = support::add_user(&state, "target", "target@example.com", false);
    let admin = support::add_user(&state, "admin", "admin@example.com", true);
    let normal = support::add_user(&state, "normal", "normal@example.com", false);
    let outsider = support::add_user(&state, "outsider", "outsider@example.com", false);
    support::associate(&state, &org, &target);
    support::associate(&state, &org, &admin);
    support::associate(&state, &org, &normal);
    let app = support::app(state);

    for user in ["pivotal", "admin", "normal", "target"] {
        let (status, _) = get(&app, "/users/target", user).await;
        assert_eq!(status, StatusCode::OK, "user {user}");
    }
    let (status, _) = get(&app, "/users/target", "outsider").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!outsider.admin);

    let (status, _) = get(&app, "/users/bogus", "pivotal").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_user_updates_profile_password_and_renames() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let body = br#"{"username":"alice","email":"new@example.com","first_name":"Alice","last_name":"Smith","display_name":"Alice Smith","password":"bidgerbidger","bogus":"ignored"}"#;
    let (status, _, updated) = send(&app, "PUT", "/users/alice", body, "2", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(updated, json!({"uri": "http://0.0.0.0:8000/users/alice"}));

    let (_, user) = get(&app, "/users/alice", "pivotal").await;
    assert_eq!(user["email"], "new@example.com");
    assert_eq!(user["first_name"], "Alice");
    assert_eq!(user["last_name"], "Smith");
    assert_eq!(user["display_name"], "Alice Smith");

    let rename = br#"{"username":"alice2","email":"new@example.com","display_name":"Alice Smith"}"#;
    let (status, headers, renamed) =
        send(&app, "PUT", "/users/alice", rename, "2", "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(renamed, json!({"uri": "http://0.0.0.0:8000/users/alice2"}));
    assert_eq!(
        headers.get(header::LOCATION).unwrap(),
        "http://0.0.0.0:8000/users/alice2"
    );
    let (status, _) = get(&app, "/users/alice", "pivotal").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, user) = get(&app, "/users/alice2", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(user["username"], "alice2");
}

#[tokio::test]
async fn update_user_rejects_invalid_bodies_and_rename_conflicts() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "alice", "alice@example.com", false);
    support::add_user(&state, "bob", "bob@example.com", false);
    let app = support::app(state);

    for body in [
        br#"{"username":"alice","email":"alice@example.com","first_name":"Alice"}"#.as_slice(),
        br#"{"username":"alice","first_name":"Alice","display_name":"Alice"}"#.as_slice(),
        br#"{"email":"alice@example.com","display_name":"Alice"}"#.as_slice(),
        br#"{"username":"alice","email":"bad @ email","display_name":"Alice"}"#.as_slice(),
        br#"{"username":"Alice","email":"alice@example.com","display_name":"Alice"}"#.as_slice(),
        br#"{"username":"alice with space","email":"alice@example.com","display_name":"Alice"}"#
            .as_slice(),
        r#"{"username":"テスト","email":"alice@example.com","display_name":"Alice"}"#.as_bytes(),
    ] {
        let (status, _, _) = send(&app, "PUT", "/users/alice", body, "2", "pivotal").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "body: {}",
            String::from_utf8_lossy(body)
        );
    }

    let rename = br#"{"username":"bob","email":"alice@example.com","display_name":"Alice"}"#;
    let (status, _, _) = send(&app, "PUT", "/users/alice", rename, "2", "pivotal").await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = get(&app, "/users/alice", "pivotal").await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = send(&app, "PUT", "/users/alice", rename, "2", "pivotal").await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _, _) = send(
        &app,
        "PUT",
        "/users/bogus",
        br#"{"username":"bogus","email":"b@example.com","display_name":"Bogus"}"#,
        "2",
        "pivotal",
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn update_external_auth_user_without_email() {
    let app = support::app(support::state_with_pivotal());
    let create = br#"{"username":"saml","display_name":"SAML","external_authentication_uid":"sam.l.jackson"}"#;
    let (status, _, _) = send(&app, "POST", "/users", create, "2", "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);

    let update = br#"{"username":"saml","display_name":"SAML User"}"#;
    let (status, _, _) = send(&app, "PUT", "/users/saml", update, "2", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn update_rejects_key_fields_in_v1() {
    let app = support::app(support::state_with_pivotal());
    let body = br#"{"username":"alice","email":"alice@example.com","display_name":"Alice","password":"secret"}"#;
    let (status, _, _) = send(&app, "POST", "/users", body, "2", "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);

    let bodies = [
        format!(
            r#"{{"username":"alice","email":"alice@example.com","display_name":"Alice","public_key":{}}}"#,
            json!(support::PUBLIC_KEY)
        ),
        r#"{"username":"alice","email":"alice@example.com","display_name":"Alice","create_key":true}"#.to_string(),
        r#"{"username":"alice","email":"alice@example.com","display_name":"Alice","private_key":true}"#.to_string(),
    ];
    for body in bodies {
        let (status, _, _) =
            send(&app, "PUT", "/users/alice", body.as_bytes(), "2", "pivotal").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    }
}

#[tokio::test]
async fn v0_update_accepts_public_key_and_rotates_private_key() {
    let app = support::app(support::state_with_pivotal());
    let create = br#"{"username":"alice","email":"alice@example.com","display_name":"Alice","password":"secret"}"#;
    let (status, _, created) = send(&app, "POST", "/users", create, "0", "pivotal").await;
    assert_eq!(status, StatusCode::CREATED);
    let original_private_key = created["private_key"].as_str().unwrap().to_string();

    let (_, new_public_key) = generate_rsa_keypair().unwrap();
    let body = format!(
        r#"{{"username":"alice","email":"alice@example.com","display_name":"Alice","public_key":{}}}"#,
        json!(new_public_key)
    );
    let (status, _, _) = send(&app, "PUT", "/users/alice", body.as_bytes(), "0", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    let (_, user) = get_with_version(&app, "/users/alice", "pivotal", "0").await;
    assert_eq!(user["public_key"], new_public_key);

    let rotate = br#"{"username":"alice","email":"alice@example.com","display_name":"Alice","private_key":true}"#;
    let (status, _, rotated) = send(&app, "PUT", "/users/alice", rotate, "0", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    let rotated_private_key = rotated["private_key"].as_str().unwrap();
    assert_ne!(rotated_private_key, original_private_key);
    let (_, user) = get_with_version(&app, "/users/alice", "pivotal", "0").await;
    assert_ne!(user["public_key"], new_public_key);

    let certificate = br#"{"username":"alice","email":"alice@example.com","display_name":"Alice","public_key":"-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n"}"#;
    let (status, _, _) = send(&app, "PUT", "/users/alice", certificate, "0", "pivotal").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn delete_user_requires_the_superuser() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "admin", "admin@example.com", true);
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let (status, _, _) = send(&app, "DELETE", "/users/alice", b"", "2", "admin").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = get(&app, "/users/alice", "pivotal").await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = send(&app, "DELETE", "/users/alice", b"", "2", "pivotal").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get(&app, "/users/alice", "pivotal").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _, _) = send(&app, "DELETE", "/users/bogus", b"", "2", "pivotal").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
