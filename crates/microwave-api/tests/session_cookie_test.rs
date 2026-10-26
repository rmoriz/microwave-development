mod support;

use axum::http::StatusCode;
use tower::ServiceExt;

async fn signin_cookie(config: microwave_config::Config) -> String {
    let state = support::state_with_config(config);
    let response = support::app(state)
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/id/signin")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response
        .headers()
        .get(axum::http::header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn session_cookies_are_secure_when_tls_is_enabled() {
    let mut config = microwave_config::Config::default();
    config.server.tls.enabled = Some(true);
    let cookie = signin_cookie(config).await;
    assert!(
        cookie.contains("Secure"),
        "TLS cookie must be Secure: {cookie}"
    );
}

#[tokio::test]
async fn session_cookies_are_usable_over_plain_http() {
    let cookie = signin_cookie(microwave_config::Config::default()).await;
    assert!(
        !cookie.contains("Secure"),
        "plain HTTP cookie must not set Secure: {cookie}"
    );
}
