mod support;

use axum::body::Body;
use axum::http::{header, HeaderName, HeaderValue, Request, StatusCode};
use microwave_crypto::SignVersion;
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
    // The loop is already headerless, so these requests are browser requests.
    // Spell the two paths out by name so a regression that answers them with
    // the page fails here instead of only showing up as a content-type
    // mismatch inside the loop.
    for (path, content_type) in [
        ("/css/tokens.css", "text/css; charset=utf-8"),
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
    for path in ["/index.html", "/"] {
        let response = app.clone().oneshot(headerless("HEAD", path)).await.unwrap();
        assert_static_headers(&response, "text/html; charset=utf-8");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.is_empty(), "{path}");
    }
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

#[tokio::test]
async fn the_root_answers_a_browser_with_the_page() {
    let response = support::app(support::state_with_pivotal())
        .oneshot(headerless("GET", "/"))
        .await
        .unwrap();
    assert_static_headers(&response, "text/html; charset=utf-8");
    assert_eq!(response.headers()["vary"], "X-Chef-Version, X-Cinc-Version");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let html = String::from_utf8(body.to_vec()).unwrap();
    assert!(html.contains("https://rmoriz.github.io/microwave-development/"));
    assert!(html.contains(r#"<link rel="icon""#));
    assert_eq!(html.matches("<h1").count(), 1);
}

#[tokio::test]
async fn an_unknown_path_answers_a_browser_with_the_page() {
    let response = support::app(support::state_with_pivotal())
        .oneshot(headerless("GET", "/nope"))
        .await
        .unwrap();
    assert_static_headers(&response, "text/html; charset=utf-8");
}

#[tokio::test]
async fn a_chef_version_header_keeps_the_api_behavior() {
    let app = support::app(support::state_with_pivotal());
    // Upstream `dispatch.lua` answers `401` for a client version header without
    // `X-Ops-Userid` before it resolves any route, so an unknown path is no
    // exception.
    for path in ["/", "/nope"] {
        let mut request = headerless("GET", path);
        request.headers_mut().insert(
            HeaderName::from_static("x-chef-version"),
            HeaderValue::from_static("19.0.0"),
        );
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    }
}

#[tokio::test]
async fn x_cinc_version_runs_the_same_minimum_matrix() {
    let app = support::app(support::state_with_pivotal());
    for (version, expected) in [
        ("9.0.1", StatusCode::BAD_REQUEST),
        ("0.9.1", StatusCode::BAD_REQUEST),
        ("0.10.1", StatusCode::OK),
        ("19.0.0", StatusCode::OK),
    ] {
        let mut request =
            support::signed_request("GET", "/users", b"", SignVersion::V1_3, "pivotal");
        request.headers_mut().insert(
            HeaderName::from_static("x-cinc-version"),
            HeaderValue::from_str(version).unwrap(),
        );
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected, "X-Cinc-Version {version}");
    }
}

#[tokio::test]
async fn chef_version_wins_over_cinc_version() {
    let app = support::app(support::state_with_pivotal());
    let mut request = support::signed_request("GET", "/users", b"", SignVersion::V1_3, "pivotal");
    request.headers_mut().insert(
        HeaderName::from_static("x-chef-version"),
        HeaderValue::from_static("9.0.1"),
    );
    request.headers_mut().insert(
        HeaderName::from_static("x-cinc-version"),
        HeaderValue::from_static("19.0.0"),
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_partial_signature_is_not_a_browser() {
    let app = support::app(support::state_with_pivotal());
    let mut request = headerless("GET", "/users");
    request.headers_mut().insert(
        HeaderName::from_static("x-ops-userid"),
        HeaderValue::from_static("pivotal"),
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(
        response.headers()[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
}

#[tokio::test]
async fn a_complete_signature_without_a_version_header_stays_on_the_api() {
    let app = support::app(support::state_with_pivotal());
    let request = support::signed_request("GET", "/users", b"", SignVersion::V1_3, "pivotal");
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let request = support::signed_request("GET", "/nope", b"", SignVersion::V1_3, "pivotal");
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_ne!(
        response.headers()[header::CONTENT_TYPE],
        "text/html; charset=utf-8"
    );
}

#[tokio::test]
async fn exempt_paths_never_serve_the_page() {
    let app = support::app(support::state_with_pivotal());
    for path in [
        "/_status",
        "/_stats",
        "/server_api_version",
        "/id",
        "/organizations/o/mcp",
    ] {
        let response = app.clone().oneshot(headerless("GET", path)).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        // `/id` is oc-id's own HTML page, so the content type does not
        // discriminate; the embedded site link only appears on the landing page.
        assert!(
            !String::from_utf8_lossy(&body)
                .contains("https://rmoriz.github.io/microwave-development/"),
            "{path}"
        );
    }
}

#[tokio::test]
async fn maintenance_mode_still_shows_the_page() {
    let state = support::state_with_pivotal();
    state.sqlite().set_maintenance_enabled(true).unwrap();
    let app = support::app(state);
    let response = app.clone().oneshot(headerless("GET", "/")).await.unwrap();
    assert_static_headers(&response, "text/html; charset=utf-8");
    let mut request = headerless("GET", "/");
    request.headers_mut().insert(
        HeaderName::from_static("x-chef-version"),
        HeaderValue::from_static("19.0.0"),
    );
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn every_surface_serves_the_same_favicon() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source =
        std::fs::read(root.join("crates/microwave-api/assets/landing/favicon.svg")).unwrap();
    for copy in [
        "website/homepage/public/favicon.svg",
        "website/docs/static/img/favicon.svg",
    ] {
        assert_eq!(std::fs::read(root.join(copy)).unwrap(), source, "{copy}");
    }
}
