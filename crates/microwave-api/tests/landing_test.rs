mod support;

use axum::body::Body;
use axum::http::{header, HeaderName, HeaderValue, Request, StatusCode};
use tower::ServiceExt;

const EXPECTED_CSP: &str = "default-src 'none'; style-src 'self'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

fn headerless(method: &str, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn assert_static_headers(response: &axum::response::Response, content_type: &str) {
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers();
    assert_eq!(headers[header::CONTENT_TYPE], content_type);
    assert_eq!(headers["content-security-policy"], EXPECTED_CSP);
    assert_eq!(headers["x-frame-options"], "DENY");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["x-xss-protection"], "1");
    assert_eq!(headers["referrer-policy"], "no-referrer");
}

#[tokio::test]
async fn index_page_carries_the_header_block() {
    let response = support::app(support::state_with_pivotal())
        .oneshot(headerless("GET", "/index.html"))
        .await
        .unwrap();
    assert_static_headers(&response, "text/html; charset=utf-8");
}

#[tokio::test]
async fn every_stylesheet_and_font_is_served_by_exact_name() {
    let app = support::app(support::state_with_pivotal());
    for (path, content_type) in [
        ("/css/tokens.css", "text/css; charset=utf-8"),
        ("/css/global.css", "text/css; charset=utf-8"),
        ("/css/components.css", "text/css; charset=utf-8"),
        ("/css/fonts.css", "text/css; charset=utf-8"),
        ("/css/landing.css", "text/css; charset=utf-8"),
        ("/fonts/inter-latin.woff2", "font/woff2"),
        ("/fonts/space-grotesk-latin.woff2", "font/woff2"),
        ("/favicon.svg", "image/svg+xml"),
    ] {
        let response = app.clone().oneshot(headerless("GET", path)).await.unwrap();
        assert_static_headers(&response, content_type);
    }
}

#[tokio::test]
async fn unknown_static_names_are_404_with_the_header_block() {
    let app = support::app(support::state_with_pivotal());
    for path in ["/css/nope.css", "/fonts/nope.woff2", "/css/", "/fonts/"] {
        let response = app.clone().oneshot(headerless("GET", path)).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    }
}

#[tokio::test]
async fn a_versions_header_does_not_suppress_static_files() {
    let app = support::app(support::state_with_pivotal());
    let mut request = headerless("GET", "/css/tokens.css");
    request.headers_mut().insert(
        HeaderName::from_static("x-chef-version"),
        HeaderValue::from_static("19.0.0"),
    );
    let response = app.oneshot(request).await.unwrap();
    assert_static_headers(&response, "text/css; charset=utf-8");
}

#[tokio::test]
async fn a_trailing_slash_still_reaches_the_stylesheet() {
    let app = support::app(support::state_with_pivotal());
    let response = app
        .oneshot(headerless("GET", "/css/tokens.css/"))
        .await
        .unwrap();
    assert_static_headers(&response, "text/css; charset=utf-8");
}

#[tokio::test]
async fn head_returns_headers_without_a_body() {
    let app = support::app(support::state_with_pivotal());
    let response = app
        .oneshot(headerless("HEAD", "/index.html"))
        .await
        .unwrap();
    assert_static_headers(&response, "text/html; charset=utf-8");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body.is_empty());
}

#[tokio::test]
async fn maintenance_mode_keeps_the_static_surface_reachable() {
    let state = support::state_with_pivotal();
    state.sqlite().set_maintenance_enabled(true).unwrap();
    let app = support::app(state);
    let page = app
        .clone()
        .oneshot(headerless("GET", "/index.html"))
        .await
        .unwrap();
    assert_static_headers(&page, "text/html; charset=utf-8");
    let stylesheet = app
        .oneshot(headerless("GET", "/css/tokens.css"))
        .await
        .unwrap();
    assert_static_headers(&stylesheet, "text/css; charset=utf-8");
}

#[tokio::test]
async fn the_page_stays_script_free_with_one_heading() {
    let app = support::app(support::state_with_pivotal());
    let response = app.oneshot(headerless("GET", "/index.html")).await.unwrap();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert_eq!(body.matches("<h1").count(), 1, "exactly one h1");
    assert!(!body.contains("<script"), "the page runs no JavaScript");
    assert!(!body.contains("style="), "no inline style attribute");
    let main = body.split_once("<main").expect("a main element").1;
    let main = main.split_once('>').expect("a closed main start tag").1;
    let main = main.split_once("</main>").expect("a closed main element").0;
    assert!(!main.trim().is_empty(), "main carries content");
}
