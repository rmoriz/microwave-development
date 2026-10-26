mod support;

use axum::http::{header, StatusCode};
use tower::ServiceExt;

async fn signin(origin: Option<&str>) -> StatusCode {
    let state = support::state_with_pivotal();
    let mut builder = axum::http::Request::builder()
        .method("POST")
        .uri("/id/sessions")
        .header(header::HOST, "chef.example.com")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    support::app(state)
        .oneshot(
            builder
                .body(axum::body::Body::from("username=x&password=y"))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn cross_origin_form_writes_are_rejected() {
    assert_eq!(
        signin(Some("https://evil.example")).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn non_browser_form_writes_without_origin_are_allowed() {
    assert_ne!(signin(None).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn same_origin_form_writes_reach_the_handler() {
    let status = signin(Some("http://chef.example.com")).await;
    assert_ne!(status, StatusCode::FORBIDDEN);
}
