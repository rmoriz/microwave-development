mod support;

use axum::http::{header, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn password_reset_tokens_are_not_returned_by_default() {
    let state = support::state_with_pivotal();
    support::add_user(&state, "alice", "alice@example.com", false);
    let app = support::app(state);

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/id/password-reset")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .header(header::ACCEPT, "application/json")
                .body(axum::body::Body::from("username=alice"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = support::response_json(response).await;
    assert!(
        body.get("reset_url").is_none(),
        "unauthenticated callers must not receive a reset token: {body}"
    );
}
