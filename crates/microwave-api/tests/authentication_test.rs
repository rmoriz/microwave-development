mod support;

use axum::http::StatusCode;
use microwave_api::AppState;
use microwave_crypto::{hash_password, verify_password, PasswordHash, SignVersion};
use microwave_store::{ActorType, User};
use serde_json::{json, Value};
use tower::ServiceExt;

fn add_user(
    state: &AppState,
    username: &str,
    password: &str,
    admin: bool,
    recovery_enabled: bool,
) -> User {
    let hash = hash_password(password).unwrap();
    let user = state
        .sqlite()
        .create_user(
            username,
            &format!("{username}@chef.io"),
            &hash.hash_type,
            &hash.hash,
            &hash.salt,
            admin,
        )
        .unwrap();
    state
        .sqlite()
        .add_actor_key(&user.id, ActorType::User, "default", support::PUBLIC_KEY)
        .unwrap();
    if recovery_enabled {
        state
            .sqlite()
            .set_recovery_authentication_enabled(username, true)
            .unwrap();
    }
    user
}

#[tokio::test]
async fn authenticate_user_returns_linked_superuser_view() {
    let state = support::state_with_pivotal();
    add_user(&state, "alice", "foobar", false, false);
    let app = support::app(state);

    let response = app
        .oneshot(support::signed_request(
            "POST",
            "/authenticate_user",
            br#"{"username":"alice","password":"foobar"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = support::response_json(response).await;
    assert_eq!(
        body,
        json!({
            "status": "linked",
            "user": {
                "username": "alice",
                "email": "alice@chef.io",
                "first_name": "alice",
                "last_name": "alice",
                "display_name": "alice",
            }
        })
    );
}

#[tokio::test]
async fn authenticate_user_rejects_bad_password() {
    let state = support::state_with_pivotal();
    add_user(&state, "alice", "foobar", false, false);
    let app = support::app(state);

    let response = app
        .oneshot(support::signed_request(
            "POST",
            "/authenticate_user",
            br#"{"username":"alice","password":"wrong"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = support::response_json(response).await;
    assert_eq!(
        body["error"],
        json!(["Failed to authenticate: Username and password incorrect"])
    );
}

#[tokio::test]
async fn authenticate_user_rejects_unknown_user() {
    let app = support::app(support::state_with_pivotal());

    let response = app
        .oneshot(support::signed_request(
            "POST",
            "/authenticate_user",
            br#"{"username":"nobody","password":"foobar"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = support::response_json(response).await;
    assert_eq!(
        body["error"],
        json!(["Failed to authenticate: Username and password incorrect"])
    );
}

#[tokio::test]
async fn authenticate_user_forbids_authenticating_the_superuser() {
    let app = support::app(support::state_with_pivotal());

    let response = app
        .oneshot(support::signed_request(
            "POST",
            "/authenticate_user",
            br#"{"username":"pivotal","password":"anything"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn authenticate_user_is_superuser_only() {
    let state = support::state_with_pivotal();
    add_user(&state, "alice", "foobar", false, false);
    let app = support::app(state);

    let response = app
        .oneshot(support::signed_request(
            "POST",
            "/authenticate_user",
            br#"{"username":"alice","password":"foobar"}"#,
            SignVersion::V1_3,
            "alice",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn authenticate_user_rejects_missing_or_empty_bodies() {
    let app = support::app(support::state_with_pivotal());

    for body in [
        b"".as_slice(),
        b"{}".as_slice(),
        br#"{"username":"alice"}"#.as_slice(),
        br#"{"password":"foobar"}"#.as_slice(),
    ] {
        let response = app
            .clone()
            .oneshot(support::signed_request(
                "POST",
                "/authenticate_user",
                body,
                SignVersion::V1_3,
                "pivotal",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "body {body:?}");
        let parsed = support::response_json(response).await;
        assert_eq!(
            parsed,
            json!({"error": ["invalid user authentication request"]})
        );
    }
}

#[tokio::test]
async fn malformed_credential_bodies_are_rejected_before_authentication() {
    use axum::body::Body;
    use axum::http::Request;

    let app = support::app(support::state_with_pivotal());

    for (path, body, message) in [
        (
            "/authenticate_user",
            "",
            "invalid user authentication request",
        ),
        ("/system_recovery", "", "Invalid system recovery request"),
        (
            "/system_recovery",
            r#"{"password":"foobar"}"#,
            "Field 'username' missing",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{path} {body:?}"
        );
        assert_eq!(
            support::response_json(response).await["error"],
            json!([message])
        );
    }

    let response = app
        .oneshot(
            Request::post("/authenticate_user")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"alice","password":"foobar"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn unsupported_credential_methods_return_405_without_authenticating() {
    use axum::body::Body;
    use axum::http::Request;

    let app = support::app(support::state_with_pivotal());

    for method in ["GET", "PUT", "DELETE"] {
        for path in ["/authenticate_user", "/system_recovery"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} {path}"
            );
        }
    }
}

#[tokio::test]
async fn authenticate_user_upgrades_legacy_passwords() {
    let state = support::state_with_pivotal();
    // Ancient "ec" SHA1 hash of "secret" with salt "mysalt".
    state
        .sqlite()
        .create_user(
            "legacy",
            "legacy@chef.io",
            "ec",
            "51bbc1a1cf81f721a87db4c4d054452dbe9ea19e",
            "mysalt",
            false,
        )
        .unwrap();
    let app = support::app(state.clone());

    let response = app
        .oneshot(support::signed_request(
            "POST",
            "/authenticate_user",
            br#"{"username":"legacy","password":"secret"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let user = state.sqlite().find_user("legacy").unwrap().unwrap();
    assert_eq!(user.hash_type, "bcrypt");
    assert!(verify_password(
        "secret",
        &PasswordHash {
            hash: user.password_hash,
            salt: user.password_salt,
            hash_type: user.hash_type,
        }
    ));
}

#[tokio::test]
async fn system_recovery_returns_the_recovery_user_without_public_key() {
    let state = support::state_with_pivotal();
    add_user(&state, "alice", "foobar", false, true);
    let app = support::app(state);

    let response = app
        .oneshot(support::signed_request(
            "POST",
            "/system_recovery",
            br#"{"username":"alice","password":"foobar"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = support::response_json(response).await;
    assert_eq!(
        body,
        json!({
            "username": "alice",
            "email": "alice@chef.io",
            "first_name": "alice",
            "last_name": "alice",
            "display_name": "alice",
            "recovery_authentication_enabled": true,
        })
    );
    assert!(body.get("public_key").is_none());
}

#[tokio::test]
async fn system_recovery_rejects_disabled_and_unknown_users() {
    let state = support::state_with_pivotal();
    add_user(&state, "alice", "foobar", false, false);
    let app = support::app(state);

    for username in ["alice", "nobody"] {
        let body = format!(r#"{{"username":"{username}","password":"foobar"}}"#);
        let leaked: &'static [u8] = Box::leak(body.into_bytes().into_boxed_slice());
        let response = app
            .clone()
            .oneshot(support::signed_request(
                "POST",
                "/system_recovery",
                leaked,
                SignVersion::V1_3,
                "pivotal",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "user {username}");
        let parsed = support::response_json(response).await;
        assert_eq!(
            parsed["error"],
            json!(["System recovery disabled for this user"])
        );
    }
}

#[tokio::test]
async fn system_recovery_rejects_bad_passwords_and_non_superusers() {
    let state = support::state_with_pivotal();
    add_user(&state, "alice", "foobar", false, true);
    let app = support::app(state);

    let response = app
        .clone()
        .oneshot(support::signed_request(
            "POST",
            "/system_recovery",
            br#"{"username":"alice","password":"wrong"}"#,
            SignVersion::V1_3,
            "pivotal",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = support::response_json(response).await;
    assert_eq!(
        body["error"],
        json!(["Failed to authenticate: Username and password incorrect"])
    );

    let response = app
        .oneshot(support::signed_request(
            "POST",
            "/system_recovery",
            br#"{"username":"alice","password":"foobar"}"#,
            SignVersion::V1_3,
            "alice",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn system_recovery_reports_missing_fields() {
    let app = support::app(support::state_with_pivotal());

    for (body, message) in [
        (
            br#"{"password":"foobar"}"#.as_slice(),
            "Field 'username' missing",
        ),
        (
            br#"{"username":"alice"}"#.as_slice(),
            "Field 'password' missing",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(support::signed_request(
                "POST",
                "/system_recovery",
                body,
                SignVersion::V1_3,
                "pivotal",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let parsed = support::response_json(response).await;
        assert_eq!(parsed["error"], json!([message]));
    }
}

async fn create_user_with_version(
    state: AppState,
    api_version: &'static str,
    body: &'static [u8],
) -> (StatusCode, Value) {
    let app = support::app(state);
    let response = app
        .oneshot(support::signed_request_with_api_version(
            "POST",
            "/users",
            body,
            SignVersion::V1_3,
            "pivotal",
            api_version,
        ))
        .await
        .unwrap();
    let status = response.status();
    (status, support::response_json(response).await)
}

#[tokio::test]
async fn user_creation_key_defaults_differ_by_api_version() {
    let body = br#"{"username":"alice","email":"alice@chef.io","display_name":"alice","password":"secret"}"#;

    let (status, v0) = create_user_with_version(support::state_with_pivotal(), "0", body).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(v0["uri"], "http://0.0.0.0:8000/users/alice");
    assert!(v0["private_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PRIVATE KEY"));
    assert!(v0.get("chef_key").is_none());

    let (status, v1) = create_user_with_version(support::state_with_pivotal(), "1", body).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(v1, json!({"uri": "http://0.0.0.0:8000/users/alice"}));
}

#[tokio::test]
async fn user_creation_uses_a_supplied_public_key() {
    let public_key = support::PUBLIC_KEY.replace('\n', "\\n");
    let body: &'static [u8] = Box::leak(
        format!(
            r#"{{"username":"alice","email":"alice@chef.io","display_name":"alice","password":"secret","public_key":"{public_key}"}}"#
        )
        .into_bytes()
        .into_boxed_slice(),
    );

    let (status, v1) = create_user_with_version(support::state_with_pivotal(), "1", body).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(v1["chef_key"]["name"], "default");
    assert!(v1["chef_key"].get("private_key").is_none());
    assert_eq!(v1["chef_key"]["public_key"], support::PUBLIC_KEY);

    let (status, v0) = create_user_with_version(support::state_with_pivotal(), "0", body).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(v0["uri"], "http://0.0.0.0:8000/users/alice");
    assert!(v0.get("private_key").is_none());
    assert!(v0.get("chef_key").is_none());
}

#[tokio::test]
async fn user_creation_rejects_conflicting_key_flags() {
    let public_key = support::PUBLIC_KEY.replace('\n', "\\n");
    let body: &'static [u8] = Box::leak(
        format!(
            r#"{{"username":"alice","email":"alice@chef.io","display_name":"alice","password":"secret","public_key":"{public_key}","create_key":true}}"#
        )
        .into_bytes()
        .into_boxed_slice(),
    );
    let (status, _) = create_user_with_version(support::state_with_pivotal(), "1", body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn x_chef_version_enforces_the_upstream_minimum() {
    let app = support::app(support::state_with_pivotal());

    for (version, expected) in [
        ("9.0.1", StatusCode::BAD_REQUEST),
        ("0.9.1", StatusCode::BAD_REQUEST),
        ("0.10.1", StatusCode::OK),
        ("10.0.0", StatusCode::OK),
        ("999.0.0", StatusCode::OK),
    ] {
        let mut request =
            support::signed_request("GET", "/users", b"", SignVersion::V1_3, "pivotal");
        request
            .headers_mut()
            .insert("x-chef-version", version.parse().unwrap());
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected, "X-Chef-Version {version}");
    }
}
