mod support;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use microwave_api::AppState;
use microwave_config::Config;
use microwave_crypto::hash_password;
use microwave_objectstore::{LocalObjectStore, ObjectStore};
use microwave_store::SqliteStore;
use std::sync::Arc;
use support::RecordingMailer;
use tower::ServiceExt;

fn build_test_state(
    mailer_enabled: bool,
    mailer_fail: bool,
    token_in_response: bool,
    public_url: Option<&str>,
) -> (AppState, Arc<RecordingMailer>) {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let hash = hash_password("secret-password").unwrap();
    let _alice = store
        .create_user(
            "alice",
            "alice@example.com",
            &hash.hash_type,
            &hash.hash,
            &hash.salt,
            false,
        )
        .unwrap();

    let mut config = Config::default();
    support::disable_keygen(&mut config);
    config.auth.password_reset_token_in_response = token_in_response;
    if let Some(url) = public_url {
        config.server.public_url = Some(url.to_string());
    }
    config.mail.from = Some("noreply@example.com".to_string());
    config.mail.reply_to = Some("support@example.com".to_string());
    config.object_store.secret = Some("test".into());

    let object_store: Arc<dyn ObjectStore + Send + Sync> = Arc::new(LocalObjectStore::new(
        std::env::temp_dir().join("mw-test-mail"),
        "test",
        "http://0.0.0.0:8000",
    ));

    let mailer = Arc::new(RecordingMailer {
        messages: Arc::new(std::sync::Mutex::new(Vec::new())),
        enabled: mailer_enabled,
        fail: mailer_fail,
    });

    let state = AppState::from_sqlite(store, Arc::new(config), object_store)
        .unwrap()
        .with_mailer(mailer.clone());

    (state, mailer)
}

async fn post_password_reset(app: &axum::Router, username: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/id/password-reset")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ACCEPT, "application/json")
                .body(Body::from(format!("username={username}")))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn sends_mail_when_enabled_and_flag_is_false() {
    let (state, mailer) = build_test_state(true, false, false, Some("https://chef.example.com"));
    let app = support::app(state);

    let response = post_password_reset(&app, "alice").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body: serde_json::Value = support::response_json(response).await;
    assert!(
        body.get("reset_url").is_none(),
        "reset_url must not be in response when flag is false"
    );

    let messages = mailer.messages.lock().unwrap();
    assert_eq!(messages.len(), 1, "exactly one mail should be sent");
    let msg = &messages[0];
    assert_eq!(msg.to, "alice@example.com");
    assert_eq!(msg.from, "noreply@example.com");
    assert_eq!(msg.reply_to.as_deref(), Some("support@example.com"));
    assert!(
        msg.text_body.contains("/id/password-reset?token="),
        "mail body must contain reset link"
    );
}

#[tokio::test]
async fn sends_mail_and_includes_url_when_flag_is_true() {
    let (state, mailer) = build_test_state(true, false, true, Some("https://chef.example.com"));
    let app = support::app(state);

    let response = post_password_reset(&app, "alice").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body: serde_json::Value = support::response_json(response).await;
    let reset_url = body["reset_url"]
        .as_str()
        .expect("reset_url should be present when flag is true");
    assert!(
        reset_url.starts_with("https://chef.example.com/id/password-reset?token="),
        "reset_url should be absolute: {reset_url}"
    );

    let messages = mailer.messages.lock().unwrap();
    assert_eq!(messages.len(), 1, "exactly one mail should be sent");
}

#[tokio::test]
async fn unknown_user_does_not_send_mail() {
    let (state, mailer) = build_test_state(true, false, false, Some("https://chef.example.com"));
    let app = support::app(state);

    let response = post_password_reset(&app, "unknown").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body: serde_json::Value = support::response_json(response).await;
    assert!(body.get("reset_url").is_none());

    let messages = mailer.messages.lock().unwrap();
    assert_eq!(messages.len(), 0, "no mail should be sent for unknown user");
}

#[tokio::test]
async fn user_without_email_does_not_send_mail() {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let hash = hash_password("secret-password").unwrap();
    let _bob = store
        .create_user(
            "bob",
            "", // no email
            &hash.hash_type,
            &hash.hash,
            &hash.salt,
            false,
        )
        .unwrap();

    let mut config = Config::default();
    support::disable_keygen(&mut config);
    config.auth.password_reset_token_in_response = false;
    config.server.public_url = Some("https://chef.example.com".to_string());
    config.mail.from = Some("noreply@example.com".to_string());
    config.object_store.secret = Some("test".into());

    let object_store: Arc<dyn ObjectStore + Send + Sync> = Arc::new(LocalObjectStore::new(
        std::env::temp_dir().join("mw-test-mail-noemail"),
        "test",
        "http://0.0.0.0:8000",
    ));

    let mailer = Arc::new(RecordingMailer {
        messages: Arc::new(std::sync::Mutex::new(Vec::new())),
        enabled: true,
        fail: false,
    });

    let state = AppState::from_sqlite(store, Arc::new(config), object_store)
        .unwrap()
        .with_mailer(mailer.clone());
    let app = support::app(state);

    let response = post_password_reset(&app, "bob").await;
    assert_eq!(response.status(), StatusCode::OK);

    let messages = mailer.messages.lock().unwrap();
    assert_eq!(
        messages.len(),
        0,
        "no mail should be sent when user has no email"
    );
}

#[tokio::test]
async fn mailer_error_does_not_change_response() {
    let (state, _mailer) = build_test_state(true, true, false, Some("https://chef.example.com"));
    let app = support::app(state);

    let response = post_password_reset(&app, "alice").await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "mailer error should not cause 5xx"
    );

    let body: serde_json::Value = support::response_json(response).await;
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn mail_disabled_does_not_send_but_flag_still_works() {
    let (state, mailer) = build_test_state(false, false, true, Some("https://chef.example.com"));
    let app = support::app(state);

    let response = post_password_reset(&app, "alice").await;
    assert_eq!(response.status(), StatusCode::OK);

    let body: serde_json::Value = support::response_json(response).await;
    assert!(
        body.get("reset_url").is_some(),
        "reset_url should be present when flag is true even if mail disabled"
    );

    let messages = mailer.messages.lock().unwrap();
    assert_eq!(messages.len(), 0, "no mail should be sent when disabled");
}

#[tokio::test]
async fn expired_token_is_rejected() {
    let store = Arc::new(SqliteStore::open_in_memory().unwrap());
    let hash = hash_password("secret-password").unwrap();
    let _alice = store
        .create_user(
            "alice",
            "alice@example.com",
            &hash.hash_type,
            &hash.hash,
            &hash.salt,
            false,
        )
        .unwrap();

    let mut config = Config::default();
    support::disable_keygen(&mut config);
    config.auth.password_reset_ttl_seconds = 0; // bypass validation
    config.object_store.secret = Some("test".into());

    let object_store: Arc<dyn ObjectStore + Send + Sync> = Arc::new(LocalObjectStore::new(
        std::env::temp_dir().join("mw-test-mail-expired"),
        "test",
        "http://0.0.0.0:8000",
    ));

    let state = AppState::from_sqlite(store, Arc::new(config), object_store).unwrap();

    // Create a token directly
    let token = state.sqlite().create_password_reset_token("alice").unwrap();

    let app = support::app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/id/password-reset?token={token}"))
                .header(header::ACCEPT, "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value = support::response_json(response).await;
    assert_eq!(
        body["valid_token"], false,
        "token with TTL=0 should be invalid"
    );
}
