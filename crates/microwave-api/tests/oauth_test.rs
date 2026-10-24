mod support;

use axum::body::Body;
use axum::http::{header, HeaderValue, Request, StatusCode};
use axum::Router;
use microwave_api::AppState;
use microwave_crypto::hash_password;
use microwave_store::{ActorType, SqliteStore, UserPatch};
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

const PASSWORD: &str = "secret-password";

struct Fixture {
    state: AppState,
    admin_id: String,
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
    store
        .add_actor_key(&admin.id, ActorType::User, "default", support::PUBLIC_KEY)
        .unwrap();
    store
        .add_actor_key(&alice.id, ActorType::User, "default", support::PUBLIC_KEY)
        .unwrap();
    let org = microwave_authz::create_organization(
        store.as_ref(),
        "acme",
        "Acme Corp",
        &admin.id,
        &admin.authz_id,
    )
    .await
    .unwrap();
    store.add_user_to_org(&org.id, &alice.id).unwrap();

    let mut config = microwave_config::Config::default();
    config.object_store.secret = Some("test".into());
    let object_store: Arc<dyn microwave_objectstore::ObjectStore + Send + Sync> =
        Arc::new(microwave_objectstore::LocalObjectStore::new(
            std::env::temp_dir().join("mw-oauth-test"),
            "test",
            "http://0.0.0.0:8000",
        ));

    Fixture {
        state: AppState::from_sqlite(store, Arc::new(config), object_store).unwrap(),
        admin_id: admin.id,
    }
}

fn app(fixture: &Fixture) -> Router {
    support::app(fixture.state.clone())
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: &str,
    content_type: &str,
    headers: &[(&str, String)],
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, content_type);
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

/// Extract the value of the `csrf-token` meta tag using the same shape Pedant
/// scrapes: `<meta name="csrf-token" content="..." />`.
fn csrf_token_from(html: &str) -> String {
    let marker = "<meta name=\"csrf-token\" content=\"";
    let start = html.find(marker).expect("csrf meta tag") + marker.len();
    let rest = &html[start..];
    let end = rest.find('"').expect("csrf token terminator");
    rest[..end].to_string()
}

/// GET /id/signin exactly like Pedant: scrape the pre-session cookie and the
/// CSRF token from the page.
async fn signin_page(app: &Router) -> (String, String, String) {
    let response = request(
        app,
        "GET",
        "/id/signin",
        "",
        "",
        &[("accept", "text/html".to_string())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = cookie_from(&response);
    let html = body_text(response).await;
    let token = csrf_token_from(&html);
    assert!(!token.is_empty());
    (cookie, token, html)
}

/// Complete Pedant's sign-in helper: GET the form, then POST the credentials
/// and CSRF token to the callback route. Returns the authenticated cookie.
async fn pedant_signin(app: &Router, username: &str, password: &str) -> String {
    let (pre_session, token, _) = signin_page(app).await;
    let body = format!(
        "username={username}&password={password}&authenticity_token={token}&commit=Sign+In"
    );
    let response = request(
        app,
        "POST",
        "/id/auth/chef/callback",
        &body,
        "application/x-www-form-urlencoded",
        &[(header::COOKIE.as_str(), pre_session)],
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::FOUND,
        "sign-in should redirect"
    );
    cookie_from(&response)
}

fn cookie_from(response: &axum::response::Response) -> String {
    response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string()
}

async fn sign_in(
    app: &Router,
    username: &str,
    password: &str,
) -> (StatusCode, axum::response::Response) {
    let body = format!("username={username}&password={password}");
    let response = request(
        app,
        "POST",
        "/id/sessions",
        &body,
        "application/x-www-form-urlencoded",
        &[("accept", "application/json".to_string())],
    )
    .await;
    let status = response.status();
    (status, response)
}

fn admin_bearer(fixture: &Fixture) -> String {
    let application = fixture
        .state
        .sqlite()
        .create_oauth_application("Bootstrap", "https://bootstrap.example/cb", "read", true)
        .unwrap();
    fixture
        .state
        .sqlite()
        .create_oauth_access_token(&fixture.admin_id, application.id, "read", Some(3600), false)
        .unwrap()
        .token
}

#[tokio::test]
async fn signs_in_with_password_and_reads_profile() {
    let fixture = fixture().await;
    let app = app(&fixture);

    let (status, response) = sign_in(&app, "alice", PASSWORD).await;
    assert_eq!(status, StatusCode::OK);
    let cookie = cookie_from(&response);
    let signed_in = body_json(response).await;
    assert_eq!(signed_in["username"], "alice");
    assert!(!signed_in["session_token"].as_str().unwrap().is_empty());

    let profile = request(
        &app,
        "GET",
        "/id/profile",
        "",
        "",
        &[
            ("accept", "application/json".to_string()),
            (header::COOKIE.as_str(), cookie),
        ],
    )
    .await;
    assert_eq!(profile.status(), StatusCode::OK);
    let profile = body_json(profile).await;
    assert_eq!(profile["username"], "alice");
    assert_eq!(profile["email"], "alice@example.com");
    assert!(profile["public_key"]
        .as_str()
        .unwrap()
        .contains("BEGIN PUBLIC KEY"));
}

#[tokio::test]
async fn rejects_invalid_credentials_and_unauthenticated_access() {
    let fixture = fixture().await;
    let app = app(&fixture);

    let (status, response) = sign_in(&app, "alice", "wrong-password").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(response).await["error"], "unauthorized");

    for path in ["/id/v1/me", "/id/v1/me/organizations", "/id/v1/users/alice"] {
        let response = request(&app, "GET", path, "", "", &[]).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }

    let response = request(&app, "GET", "/id/oauth/applications", "", "", &[]).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn application_management_requires_an_admin() {
    let fixture = fixture().await;
    let app = app(&fixture);

    let (_, alice_session) = sign_in(&app, "alice", PASSWORD).await;
    let alice_cookie = cookie_from(&alice_session);

    let response = request(
        &app,
        "GET",
        "/id/oauth/applications",
        "",
        "",
        &[
            ("accept", "application/json".to_string()),
            (header::COOKIE.as_str(), alice_cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let admin = format!("Bearer {}", admin_bearer(&fixture));
    let create_body = r#"{"name":"Supermarket","redirect_uri":"https://supermarket.example/cb"}"#;
    let response = request(
        &app,
        "POST",
        "/id/oauth/applications",
        create_body,
        "application/json",
        &[
            ("accept", "application/json".to_string()),
            (header::AUTHORIZATION.as_str(), admin.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = body_json(response).await;
    let id = created["id"].as_i64().unwrap();
    assert_eq!(created["name"], "Supermarket");
    assert!(created["secret"].as_str().unwrap().len() > 20);

    let response = request(
        &app,
        "GET",
        "/id/oauth/applications",
        "",
        "",
        &[
            ("accept", "application/json".to_string()),
            (header::AUTHORIZATION.as_str(), admin.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let applications = body_json(response).await;
    assert_eq!(applications.as_array().unwrap().len(), 2);

    let update_body = r#"{"name":"Renamed"}"#;
    let response = request(
        &app,
        "PUT",
        &format!("/id/oauth/applications/{id}"),
        update_body,
        "application/json",
        &[
            ("accept", "application/json".to_string()),
            (header::AUTHORIZATION.as_str(), admin.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["name"], "Renamed");

    let response = request(
        &app,
        "DELETE",
        &format!("/id/oauth/applications/{id}"),
        "",
        "",
        &[
            ("accept", "application/json".to_string()),
            (header::AUTHORIZATION.as_str(), admin.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, serde_json::json!({}));
}

#[tokio::test]
async fn authorization_code_token_refresh_and_revoke_flow() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let application = fixture
        .state
        .sqlite()
        .create_oauth_application(
            "Supermarket",
            "https://supermarket.example/cb",
            "read",
            true,
        )
        .unwrap();

    let (_, session) = sign_in(&app, "alice", PASSWORD).await;
    let cookie = cookie_from(&session);

    let authorize_path = format!(
        "/id/oauth/authorize?client_id={}&redirect_uri={}&response_type=code&state=abc",
        application.uid, "https://supermarket.example/cb"
    );
    let response = request(
        &app,
        "GET",
        &authorize_path,
        "",
        "",
        &[(header::COOKIE.as_str(), cookie.clone())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&html).contains("Authorization Required"));

    let body = format!(
        "client_id={}&redirect_uri={}&response_type=code&state=abc",
        application.uid, "https://supermarket.example/cb"
    );
    let response = request(
        &app,
        "POST",
        "/id/oauth/authorize",
        &body,
        "application/x-www-form-urlencoded",
        &[(header::COOKIE.as_str(), cookie)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(location.starts_with("https://supermarket.example/cb?code="));
    assert!(location.ends_with("&state=abc"));
    let code = location
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();

    let body = format!(
        "grant_type=authorization_code&code={code}&client_id={}&client_secret={}&redirect_uri={}",
        application.uid, application.secret, "https://supermarket.example/cb"
    );
    let response = request(
        &app,
        "POST",
        "/id/oauth/token",
        &body,
        "application/x-www-form-urlencoded",
        &[],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let token = body_json(response).await;
    let access_token = token["access_token"].as_str().unwrap().to_string();
    let refresh_token = token["refresh_token"].as_str().unwrap().to_string();
    assert_eq!(token["token_type"], "bearer");
    assert_eq!(token["scope"], "read");

    let bearer = format!("Bearer {access_token}");
    let response = request(
        &app,
        "GET",
        "/id/v1/me",
        "",
        "",
        &[(header::AUTHORIZATION.as_str(), bearer)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let me = body_json(response).await;
    assert_eq!(me["username"], "alice");
    assert_eq!(me["email"], "alice@example.com");

    let response = request(
        &app,
        "GET",
        "/id/v1/me/organizations",
        "",
        "",
        &[(
            header::AUTHORIZATION.as_str(),
            format!("Bearer {access_token}"),
        )],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let organizations = body_json(response).await;
    assert_eq!(organizations[0]["name"], "acme");

    let response = request(
        &app,
        "GET",
        "/id/v1/users/alice",
        "",
        "",
        &[(
            header::AUTHORIZATION.as_str(),
            format!("Bearer {access_token}"),
        )],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["username"], "alice");

    let body = format!(
        "grant_type=refresh_token&refresh_token={refresh_token}&client_id={}&client_secret={}",
        application.uid, application.secret
    );
    let response = request(
        &app,
        "POST",
        "/id/oauth/token",
        &body,
        "application/x-www-form-urlencoded",
        &[],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let refreshed = body_json(response).await;
    let refreshed_token = refreshed["access_token"].as_str().unwrap().to_string();
    assert_ne!(refreshed_token, access_token);

    let body = format!(
        "token={refreshed_token}&client_id={}&client_secret={}",
        application.uid, application.secret
    );
    let response = request(
        &app,
        "POST",
        "/id/oauth/revoke",
        &body,
        "application/x-www-form-urlencoded",
        &[],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = request(
        &app,
        "GET",
        "/id/v1/me",
        "",
        "",
        &[(
            header::AUTHORIZATION.as_str(),
            format!("Bearer {refreshed_token}"),
        )],
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rejects_authorization_requests_with_an_unknown_client() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let response = request(
        &app,
        "GET",
        "/id/oauth/authorize?client_id=missing&redirect_uri=https://example.test/cb&response_type=code",
        "",
        "",
        &[],
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(response).await["error"], "invalid_client");
}

#[tokio::test]
async fn status_is_public_and_matches_the_upstream_shape() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let response = request(&app, "GET", "/id/v1/status", "", "", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({
            "status": "ok",
            "erchef": { "status": "reachable" },
            "postgres": { "status": "reachable" }
        })
    );
}

#[tokio::test]
async fn changes_password_and_regenerates_a_key() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let (_, session) = sign_in(&app, "alice", PASSWORD).await;
    let cookie = cookie_from(&session);

    let body = format!(
        "current_password={PASSWORD}&new_password=new-password&password_confirmation=new-password"
    );
    let response = request(
        &app,
        "PUT",
        "/id/profile/password",
        &body,
        "application/x-www-form-urlencoded",
        &[
            ("accept", "application/json".to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (status, _) = sign_in(&app, "alice", "new-password").await;
    assert_eq!(status, StatusCode::OK);

    let response = request(
        &app,
        "POST",
        "/id/profile/regen_key",
        "",
        "",
        &[(header::COOKIE.as_str(), cookie)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let private_key = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&private_key).contains("BEGIN PRIVATE KEY"));
}

#[tokio::test]
async fn password_reset_request_and_update_flow() {
    let fixture = fixture().await;
    let app = app(&fixture);

    let response = request(
        &app,
        "POST",
        "/id/password-reset",
        "username=alice",
        "application/x-www-form-urlencoded",
        &[("accept", "application/json".to_string())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let reset = body_json(response).await;
    let reset_url = reset["reset_url"].as_str().unwrap().to_string();
    let token = reset_url.split("token=").nth(1).unwrap().to_string();

    let body =
        format!("token={token}&password=reset-password&password_confirmation=reset-password");
    let response = request(
        &app,
        "PUT",
        "/id/password-reset",
        &body,
        "application/x-www-form-urlencoded",
        &[("accept", "application/json".to_string())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (status, _) = sign_in(&app, "alice", "reset-password").await;
    assert_eq!(status, StatusCode::OK);

    let body = "token=invalid&password=again&password_confirmation=again";
    let response = request(
        &app,
        "PUT",
        "/id/password-reset",
        body,
        "application/x-www-form-urlencoded",
        &[],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn application_form_posts_redirect_and_render_html() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let (_, session) = sign_in(&app, "admin", PASSWORD).await;
    let cookie = cookie_from(&session);

    let response = request(
        &app,
        "POST",
        "/id/oauth/applications",
        "name=Supermarket&redirect_uri=https%3A%2F%2Fsupermarket.example%2Fcb",
        "application/x-www-form-urlencoded",
        &[(header::COOKIE.as_str(), cookie.clone())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    let location = response
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(location, "/id/oauth/applications/1");

    let response = request(
        &app,
        "GET",
        &location,
        "",
        "",
        &[(header::COOKIE.as_str(), cookie)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let html = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&html);
    assert!(html.contains("Application: Supermarket"));
    assert!(html.contains("application_id"));
}

#[tokio::test]
async fn home_and_signin_pages_are_served_without_chef_signatures() {
    let fixture = fixture().await;
    let app = app(&fixture);
    for path in ["/id", "/id/", "/id/signin"] {
        let response = request(&app, "GET", path, "", "", &[]).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/html; charset=utf-8"
        );
        if path == "/id/signin" {
            assert!(response.headers().contains_key(header::SET_COOKIE));
        }
    }
}

#[tokio::test]
async fn signin_page_exposes_the_pedant_csrf_meta_tag_and_callback_form() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let (cookie, token, html) = signin_page(&app).await;

    assert!(cookie.starts_with("_oc_id_session="));
    assert!(
        html.contains(&format!("<meta name=\"csrf-token\" content=\"{token}\" />")),
        "meta tag must be self-closing for Pedant's regex: {html}"
    );
    assert!(html.contains("action=\"/id/auth/chef/callback\""));
    assert!(html.contains(&format!("name=\"authenticity_token\" value=\"{token}\"")));
}

#[tokio::test]
async fn pedant_signin_rejects_a_bad_password_with_the_failure_redirect() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let (pre_session, token, _) = signin_page(&app).await;
    let body = format!("username=alice&password=wrong&authenticity_token={token}&commit=Sign+In");
    let response = request(
        &app,
        "POST",
        "/id/auth/chef/callback",
        &body,
        "application/x-www-form-urlencoded",
        &[(header::COOKIE.as_str(), pre_session)],
    )
    .await;

    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "/id/auth/failure?message=invalid_credentials&strategy=chef"
    );
}

#[tokio::test]
async fn pedant_signin_profile_show_update_password_and_key_reset_flow() {
    let fixture = fixture().await;
    fixture
        .state
        .sqlite()
        .update_user_patch(
            "alice",
            &UserPatch {
                first_name: Some(Some("Alice".into())),
                middle_name: Some(Some("Q".into())),
                last_name: Some(Some("Liddell".into())),
                ..Default::default()
            },
        )
        .unwrap();
    let app = app(&fixture);

    let (pre_session, token, _) = signin_page(&app).await;
    let body =
        format!("username=alice&password={PASSWORD}&authenticity_token={token}&commit=Sign+In");
    let response = request(
        &app,
        "POST",
        "/id/auth/chef/callback",
        &body,
        "application/x-www-form-urlencoded",
        &[(header::COOKIE.as_str(), pre_session)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "/id/oauth/authorized_applications"
    );
    let cookie = cookie_from(&response);

    // GET /id/profile: Pedant matches value-before-id inputs and the signed-in
    // banner.
    let response = request(
        &app,
        "GET",
        "/id/profile",
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
    assert!(html.contains("Signed in as: alice"));
    assert!(html.contains("value=\"Alice\" id=\"user_first_name\""));
    assert!(html.contains("value=\"Q\" id=\"user_middle_name\""));
    assert!(html.contains("value=\"Liddell\" id=\"user_last_name\""));
    assert!(html.contains("value=\"alice@example.com\" id=\"user_email\""));

    // POST /id/profile with _method=PUT: names update, email is left alone.
    let body = "_method=PUT&authenticity_token=token\
                &user%5Bfirst_name%5D=ALICE-FIRST\
                &user%5Bmiddle_name%5D=ALICE-MIDDLE\
                &user%5Blast_name%5D=ALICE-LAST\
                &commit=Save+Changes";
    let response = request(
        &app,
        "POST",
        "/id/profile",
        body,
        "application/x-www-form-urlencoded",
        &[
            ("accept", "text/html".to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "/id/profile"
    );
    let user = fixture.state.sqlite().find_user("alice").unwrap().unwrap();
    assert_eq!(user.first_name.as_deref(), Some("ALICE-FIRST"));
    assert_eq!(user.middle_name.as_deref(), Some("ALICE-MIDDLE"));
    assert_eq!(user.last_name.as_deref(), Some("ALICE-LAST"));
    assert_eq!(user.email.as_deref(), Some("alice@example.com"));
    // Upstream's oc-id profile update round-trips the key with a second
    // trailing newline; Pedant asserts that exact shape.
    let key_after_update = fixture
        .state
        .sqlite()
        .find_actor_key(&user.id, ActorType::User, "default")
        .unwrap()
        .unwrap();
    assert!(key_after_update.public_key.ends_with("\n\n"));
    assert_eq!(
        key_after_update.public_key.trim_end(),
        support::PUBLIC_KEY.trim_end()
    );

    // POST /id/profile/password with _method=PUT.
    let body = format!(
        "_method=PUT&current_password={PASSWORD}&new_password=barfoo\
         &password_confirmation=barfoo&authenticity_token=token"
    );
    let response = request(
        &app,
        "POST",
        "/id/profile/password",
        &body,
        "application/x-www-form-urlencoded",
        &[
            ("accept", "text/html".to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "/id/profile"
    );
    let (status, _) = sign_in(&app, "alice", "barfoo").await;
    assert_eq!(status, StatusCode::OK);

    // POST /id/profile/regen_key returns a fresh PEM attachment.
    let response = request(
        &app,
        "POST",
        "/id/profile/regen_key",
        "authenticity_token=token&commit=Get+a+New+Key",
        "application/x-www-form-urlencoded",
        &[
            ("accept", "text/html".to_string()),
            (header::COOKIE.as_str(), cookie.clone()),
        ],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let private_key = body_text(response).await;
    assert!(private_key.contains("BEGIN PRIVATE KEY"));
    let stored_key = fixture
        .state
        .sqlite()
        .find_actor_key(&user.id, ActorType::User, "default")
        .unwrap()
        .unwrap();
    assert_ne!(stored_key.public_key, support::PUBLIC_KEY);
}

#[tokio::test]
async fn signout_clears_the_identity_session() {
    let fixture = fixture().await;
    let app = app(&fixture);
    let cookie = pedant_signin(&app, "alice", PASSWORD).await;

    let response = request(
        &app,
        "DELETE",
        "/id/signout",
        "",
        "",
        &[(header::COOKIE.as_str(), cookie.clone())],
    )
    .await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "/id/signin"
    );
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
    assert_eq!(
        response.headers().get(header::LOCATION).unwrap(),
        "/id/signin"
    );
}
