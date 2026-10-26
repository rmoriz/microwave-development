//! Route-completeness coverage for the oc-id surface: PATCH aliases, the
//! Doorkeeper `new` pages, authorized-application listing/revocation, token
//! info, and the session delete alias.

mod support;

use axum::body::Body;
use axum::http::{header, HeaderValue, Request, StatusCode};
use axum::Router;
use microwave_api::AppState;
use microwave_crypto::hash_password;
use microwave_store::{OAuthRepository, SqliteStore};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

const PASSWORD: &str = "secret-password";
const FORM: &str = "application/x-www-form-urlencoded";
const JSON: &str = "application/json";

struct Fixture {
    state: AppState,
    admin_id: String,
    alice_id: String,
}

async fn fixture() -> Fixture {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let hash = hash_password(PASSWORD).unwrap();
    let admin = store
        .create_user(
            "admin",
            "admin@example.com",
            &hash.hash_type,
            &hash.hash,
            &hash.salt,
            true,
        )
        .unwrap();
    let alice = store
        .create_user(
            "alice",
            "alice@example.com",
            &hash.hash_type,
            &hash.hash,
            &hash.salt,
            false,
        )
        .unwrap();

    let mut config = microwave_config::Config::default();
    support::disable_keygen(&mut config);
    config.auth.password_reset_token_in_response = true;
    config.object_store.secret = Some("test".into());
    let object_store: Arc<dyn microwave_objectstore::ObjectStore + Send + Sync> =
        Arc::new(microwave_objectstore::LocalObjectStore::new(
            std::env::temp_dir().join(format!("mw-oauth-routes-{}", uuid::Uuid::new_v4().simple())),
            "test",
            "http://0.0.0.0:8000",
        ));

    Fixture {
        state: AppState::from_sqlite(store, Arc::new(config), object_store).unwrap(),
        admin_id: admin.id,
        alice_id: alice.id,
    }
}

fn app(fixture: &Fixture) -> Router {
    support::app(fixture.state.clone())
}

fn write_headers(method: &str) -> bool {
    matches!(method, "POST" | "PUT" | "PATCH" | "DELETE")
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: &str,
    content_type: &str,
    headers: &[(&str, String)],
) -> axum::response::Response {
    let mut builder = Request::builder().method(method).uri(path);
    if !content_type.is_empty() {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    for (name, value) in headers {
        builder = builder.header(*name, HeaderValue::from_str(value).unwrap());
    }
    if write_headers(method) {
        builder = builder
            .header(header::HOST, "0.0.0.0:8000")
            .header(header::ORIGIN, "http://0.0.0.0:8000");
    }
    app.clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

/// Deliberately sends a foreign Origin to exercise the P0 CSRF guard.
async fn cross_origin_request(
    app: &Router,
    method: &str,
    path: &str,
    body: &str,
    headers: &[(&str, String)],
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, FORM)
        .header(header::HOST, "0.0.0.0:8000")
        .header(header::ORIGIN, "http://evil.example");
    for (name, value) in headers {
        builder = builder.header(*name, HeaderValue::from_str(value).unwrap());
    }
    app.clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn body_text(response: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn cookie_from(response: &axum::response::Response) -> String {
    response
        .headers()
        .get(header::SET_COOKIE)
        .expect("set-cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

fn location(response: &axum::response::Response) -> String {
    response
        .headers()
        .get(header::LOCATION)
        .expect("location")
        .to_str()
        .unwrap()
        .to_string()
}

async fn sign_in(app: &Router, username: &str, password: &str) -> String {
    let body = format!("username={username}&password={password}");
    let response = request(
        app,
        "POST",
        "/id/sessions",
        &body,
        FORM,
        &[("accept", JSON.to_string())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "sign in {username}");
    cookie_from(&response)
}

fn create_application(
    fixture: &Fixture,
    name: &str,
    scopes: &str,
) -> microwave_store::OAuthApplication {
    fixture
        .state
        .sqlite()
        .create_oauth_application(name, &format!("https://{name}.example/cb"), scopes, true)
        .unwrap()
}

#[tokio::test]
async fn patch_aliases_match_rails_and_doorkeeper() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let cookie = sign_in(&app, "alice", PASSWORD).await;

    // PATCH /id/profile (Rails resource :profile update).
    let response = request(
        &app,
        "PATCH",
        "/id/profile",
        "user%5Bfirst_name%5D=Patchy&user%5Blast_name%5D=McPatchface",
        FORM,
        &[
            ("accept", "text/html".to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(location(&response), "/id/profile");
    let user = fixture.state.sqlite().find_user("alice").unwrap().unwrap();
    assert_eq!(user.first_name.as_deref(), Some("Patchy"));
    assert_eq!(user.last_name.as_deref(), Some("McPatchface"));

    // PATCH /id/profile/password.
    let body = format!(
        "current_password={PASSWORD}&new_password=patched-password\
         &password_confirmation=patched-password"
    );
    let response = request(
        &app,
        "PATCH",
        "/id/profile/password",
        &body,
        FORM,
        &[
            ("accept", "application/json".to_string()),
            (header::COOKIE.as_str(), cookie),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    sign_in(&app, "alice", "patched-password").await;

    // PATCH /id/password-reset (Rails resource :password_reset update).
    let response = request(
        &app,
        "POST",
        "/id/password-reset",
        "username=alice",
        FORM,
        &[("accept", JSON.to_string())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let reset = body_json(response).await;
    let token = reset["reset_url"]
        .as_str()
        .unwrap()
        .split("token=")
        .nth(1)
        .unwrap()
        .to_string();
    let body =
        format!("token={token}&password=reset-password&password_confirmation=reset-password");
    let response = request(
        &app,
        "PATCH",
        "/id/password-reset",
        &body,
        FORM,
        &[("accept", JSON.to_string())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    sign_in(&app, "alice", "reset-password").await;

    // PATCH /id/oauth/applications/:id (Doorkeeper resources :applications).
    let application = create_application(&fixture, "patchable", "read");
    let admin = sign_in(&app, "admin", PASSWORD).await;
    let response = request(
        &app,
        "PATCH",
        &format!("/id/oauth/applications/{}", application.id),
        r#"{"name":"patched-application"}"#,
        JSON,
        &[
            ("accept", JSON.to_string()),
            (header::COOKIE.as_str(), admin),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await["name"],
        Value::String("patched-application".into())
    );
}

#[tokio::test]
async fn new_pages_are_available() {
    let fixture = fixture().await;
    let app = app(&fixture);

    let response = request(
        &app,
        "GET",
        "/id/password-reset/new",
        "",
        "",
        &[("accept", "text/html".to_string())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("Reset Password"));
    assert!(html.contains("action=\"/id/password-reset\""));

    let response = request(
        &app,
        "GET",
        "/id/sessions/new",
        "",
        "",
        &[("accept", "text/html".to_string())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key(header::SET_COOKIE));
    assert!(body_text(response).await.contains("Sign In"));

    let cookie = sign_in(&app, "alice", PASSWORD).await;
    let response = request(
        &app,
        "GET",
        "/id/sessions/new",
        "",
        "",
        &[
            ("accept", "text/html".to_string()),
            (header::COOKIE.as_str(), cookie),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(location(&response), "/id/oauth/authorized_applications");

    let admin = sign_in(&app, "admin", PASSWORD).await;
    let response = request(
        &app,
        "GET",
        "/id/oauth/applications/new",
        "",
        "",
        &[
            ("accept", "text/html".to_string()),
            (header::COOKIE.as_str(), admin),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("New application"));
    assert!(html.contains("action=\"/id/oauth/applications\""));

    let alice = sign_in(&app, "alice", PASSWORD).await;
    let response = request(
        &app,
        "GET",
        "/id/oauth/applications/new",
        "",
        "",
        &[
            ("accept", JSON.to_string()),
            (header::COOKIE.as_str(), alice),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn authorized_applications_lists_and_revokes_the_current_users_grants() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let cookie = sign_in(&app, "alice", PASSWORD).await;

    let alpha = create_application(&fixture, "alpha", "read");
    let beta = create_application(&fixture, "beta", "read write");
    let alpha_token = fixture
        .state
        .sqlite()
        .create_oauth_access_token(&fixture.alice_id, alpha.id, "read write", Some(3600), false)
        .unwrap();
    fixture
        .state
        .sqlite()
        .create_oauth_access_token(&fixture.alice_id, beta.id, "read", Some(3600), false)
        .unwrap();
    // Another owner's token must never appear or be revoked.
    let admin_token = fixture
        .state
        .sqlite()
        .create_oauth_access_token(&fixture.admin_id, alpha.id, "read", Some(3600), false)
        .unwrap();

    let response = request(
        &app,
        "GET",
        "/id/oauth/authorized_applications",
        "",
        "",
        &[
            ("accept", JSON.to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let applications = body_json(response).await["applications"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(applications.len(), 2, "{applications:?}");
    let alpha_entry = applications
        .iter()
        .find(|entry| entry["name"] == "alpha")
        .expect("alpha listed");
    assert_eq!(alpha_entry["uid"], alpha.uid);
    assert_eq!(alpha_entry["scopes"], "read write");
    assert!(!alpha_entry["created_at"].as_str().unwrap().is_empty());
    assert!(applications.iter().any(|entry| entry["name"] == "beta"));

    let response = request(
        &app,
        "GET",
        "/id/oauth/authorized_applications",
        "",
        "",
        &[
            ("accept", "text/html".to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = body_text(response).await;
    assert!(html.contains("alpha"));
    assert!(html.contains("beta"));
    assert!(html.contains(&format!(
        "action=\"/id/oauth/authorized_applications/{}\"",
        alpha.id
    )));

    // DELETE revokes one application's tokens for the current owner only.
    let response = request(
        &app,
        "DELETE",
        &format!("/id/oauth/authorized_applications/{}", alpha.id),
        "",
        "",
        &[
            ("accept", JSON.to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let revoked = fixture
        .state
        .sqlite()
        .find_oauth_access_token(&alpha_token.token)
        .unwrap()
        .unwrap();
    assert!(revoked.revoked_at.is_some());
    let untouched = fixture
        .state
        .sqlite()
        .find_oauth_access_token(&admin_token.token)
        .unwrap()
        .unwrap();
    assert!(untouched.revoked_at.is_none());

    let response = request(
        &app,
        "GET",
        "/id/oauth/authorized_applications",
        "",
        "",
        &[
            ("accept", JSON.to_string()),
            (header::COOKIE.as_str(), cookie),
        ],
    )
    .await;
    let applications = body_json(response).await["applications"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(applications.len(), 1);
    assert_eq!(applications[0]["name"], "beta");
}

#[tokio::test]
async fn token_info_returns_doorkeeper_fields_and_requires_a_bearer_token() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let application = create_application(&fixture, "token-info", "read");
    let token = fixture
        .state
        .sqlite()
        .create_oauth_access_token(
            &fixture.alice_id,
            application.id,
            "read write",
            Some(3600),
            false,
        )
        .unwrap();

    let response = request(
        &app,
        "GET",
        "/id/oauth/token/info",
        "",
        "",
        &[(
            header::AUTHORIZATION.as_str(),
            format!("Bearer {}", token.token),
        )],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let info = body_json(response).await;
    assert_eq!(
        info["resource_owner_id"].as_str(),
        Some(fixture.alice_id.as_str())
    );
    assert_eq!(info["scope"], "read write");
    assert_eq!(
        info["application"]["uid"].as_str(),
        Some(application.uid.as_str())
    );
    assert!(info["created_at"].as_i64().unwrap() > 0);
    let expires_in = info["expires_in"].as_i64().unwrap();
    assert!(expires_in > 0 && expires_in <= 3600, "{expires_in}");

    // No token, an invalid token, a session cookie, or a revoked token all 401.
    for headers in [
        Vec::new(),
        vec![(header::AUTHORIZATION.as_str(), "Bearer nope".to_string())],
    ] {
        let response = request(&app, "GET", "/id/oauth/token/info", "", "", &headers).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(response).await["error"], "invalid_token");
    }

    let cookie = sign_in(&app, "alice", PASSWORD).await;
    let response = request(
        &app,
        "GET",
        "/id/oauth/token/info",
        "",
        "",
        &[(header::COOKIE.as_str(), cookie)],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "token info must not fall back to a browser session"
    );

    fixture
        .state
        .sqlite()
        .revoke_oauth_access_token(&token.token)
        .unwrap();
    let response = request(
        &app,
        "GET",
        "/id/oauth/token/info",
        "",
        "",
        &[(
            header::AUTHORIZATION.as_str(),
            format!("Bearer {}", token.token),
        )],
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    let expired = fixture
        .state
        .sqlite()
        .create_oauth_access_token(&fixture.alice_id, application.id, "read", Some(-1), false)
        .unwrap();
    let response = request(
        &app,
        "GET",
        "/id/oauth/token/info",
        "",
        "",
        &[(
            header::AUTHORIZATION.as_str(),
            format!("Bearer {}", expired.token),
        )],
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn delete_session_alias_signs_out() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let cookie = sign_in(&app, "alice", PASSWORD).await;

    let response = request(
        &app,
        "DELETE",
        "/id/sessions/anything",
        "",
        "",
        &[
            ("accept", "text/html".to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(location(&response), "/id/signin");
    assert!(response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .contains("Max-Age=0"));

    let response = request(
        &app,
        "GET",
        "/id/profile",
        "",
        "",
        &[
            ("accept", "text/html".to_string()),
            (header::COOKIE.as_str(), cookie),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(location(&response), "/id/signin");
}

#[tokio::test]
async fn csrf_guard_still_rejects_cross_origin_patch_and_delete_requests() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let cookie = sign_in(&app, "alice", PASSWORD).await;

    let response = cross_origin_request(
        &app,
        "PATCH",
        "/id/profile",
        "user%5Bfirst_name%5D=Hijacked",
        &[
            ("accept", JSON.to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let user = fixture.state.sqlite().find_user("alice").unwrap().unwrap();
    assert_ne!(user.first_name.as_deref(), Some("Hijacked"));

    let response = cross_origin_request(
        &app,
        "PATCH",
        "/id/profile/password",
        "current_password=x&new_password=y&password_confirmation=y",
        &[
            ("accept", JSON.to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = cross_origin_request(
        &app,
        "PATCH",
        "/id/password-reset",
        "token=x&password=y&password_confirmation=y",
        &[("accept", JSON.to_string())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let application = create_application(&fixture, "csrf-protected", "read");
    let token = fixture
        .state
        .sqlite()
        .create_oauth_access_token(&fixture.alice_id, application.id, "read", Some(3600), false)
        .unwrap();
    let response = cross_origin_request(
        &app,
        "DELETE",
        &format!("/id/oauth/authorized_applications/{}", application.id),
        "",
        &[
            ("accept", JSON.to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(fixture
        .state
        .sqlite()
        .find_oauth_access_token(&token.token)
        .unwrap()
        .unwrap()
        .revoked_at
        .is_none());

    let response = cross_origin_request(
        &app,
        "DELETE",
        "/id/sessions/current",
        "",
        &[
            ("accept", "text/html".to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = request(
        &app,
        "GET",
        "/id/profile",
        "",
        "",
        &[
            ("accept", "text/html".to_string()),
            (header::COOKIE.as_str(), cookie),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// The PostgreSQL backend must implement the same list/revoke semantics. The
/// test skips unless MICROWAVE_TEST_POSTGRES_URL points at a disposable
/// database.
#[tokio::test]
async fn postgres_authorized_application_queries_are_symmetric() {
    let Some(url) = std::env::var("MICROWAVE_TEST_POSTGRES_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        eprintln!(
            "SKIP: MICROWAVE_TEST_POSTGRES_URL is not set; start a disposable PostgreSQL \
             with scripts/run-postgres-tests.sh"
        );
        return;
    };
    let store = microwave_store_postgres::PostgresStore::connect(&url)
        .await
        .expect("connect to configured PostgreSQL");
    let owner = format!("pg-owner-{}", uuid::Uuid::new_v4().simple());
    let name = format!("pg-oauth-{}", uuid::Uuid::new_v4().simple());
    let application = store
        .create_oauth_application(&name, "https://pg.example/cb", "read", true)
        .await
        .expect("create application");
    let token = store
        .create_oauth_access_token(&owner, application.id, "read write", Some(3600), false)
        .await
        .expect("create token");
    let grant = store
        .create_oauth_access_grant(&owner, application.id, "https://pg.example/cb", "read", 600)
        .await
        .expect("create grant");

    let listed = store
        .list_oauth_access_tokens_for_owner(&owner)
        .await
        .expect("list tokens");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].token, token.token);
    assert_eq!(listed[0].scopes, "read write");

    store
        .revoke_oauth_tokens_and_grants_for(application.id, &owner)
        .await
        .expect("revoke tokens and grants");
    assert!(store
        .find_oauth_access_token(&token.token)
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .is_some());
    assert!(store
        .find_oauth_access_grant(&grant.token)
        .await
        .unwrap()
        .unwrap()
        .revoked_at
        .is_some());
    assert!(store
        .list_oauth_access_tokens_for_owner(&owner)
        .await
        .unwrap()
        .is_empty());

    store
        .delete_oauth_application(application.id)
        .await
        .expect("clean up application");
}
