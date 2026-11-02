//! Embedded landing page for browsers.
//!
//! A request with no client-version header must not land on the JSON `401` of
//! the signed API. Upstream nginx answers those requests with a static
//! `index.html`, and so does this module.
//!
//! Everything is compiled into the binary: the page, its assets and the fonts.
//! The three site stylesheets are embedded straight from
//! `website/homepage/src/styles/`, so the page follows the site design instead
//! of carrying a copy of its tokens.

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;

/// Where the project is documented and introduced.
pub const WEBSITE_URL: &str = "https://rmoriz.github.io/microwave-development/";
pub const CSS_FILES: [&str; 5] = [
    "tokens.css",
    "global.css",
    "components.css",
    "fonts.css",
    "landing.css",
];
pub const FONT_FILES: [&str; 2] = ["inter-latin.woff2", "space-grotesk-latin.woff2"];

const CONTENT_SECURITY_POLICY: &str = "default-src 'none'; style-src 'self'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";
const CACHE_PUBLIC: &str = "public, max-age=3600";
/// `no-transform` keeps the compression middleware from appending
/// `Accept-Encoding` to the page's `Vary`, so that header names exactly the
/// negotiation that matters here: the client version headers.
const CACHE_PAGE: &str = "no-cache, no-transform";

const PAGE: &str = include_str!("../assets/landing/index.html");
const FAVICON: &[u8] = include_bytes!("../assets/landing/favicon.svg");
const FONTS_CSS: &str = include_str!("../assets/landing/fonts.css");
const LANDING_CSS: &str = include_str!("../assets/landing/landing.css");
const TOKENS_CSS: &str = include_str!("../../../website/homepage/src/styles/tokens.css");
const GLOBAL_CSS: &str = include_str!("../../../website/homepage/src/styles/global.css");
const COMPONENTS_CSS: &str = include_str!("../../../website/homepage/src/styles/components.css");
const INTER_WOFF2: &[u8] = include_bytes!("../assets/fonts/inter-latin.woff2");
const SPACE_GROTESK_WOFF2: &[u8] = include_bytes!("../assets/fonts/space-grotesk-latin.woff2");

/// The client version headers decide between the page and the JSON API, so a
/// shared cache must key on them.
const VARY_PAGE: &str = "X-Chef-Version, X-Cinc-Version";

/// The landing page itself.
pub fn page() -> Response {
    let mut response = static_response(
        "text/html; charset=utf-8",
        CACHE_PAGE,
        Body::from(page_body()),
    );
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static(VARY_PAGE));
    response
}

/// The page with the site URL substituted. The URL lives in one constant so
/// the page and its tests cannot drift apart.
fn page_body() -> String {
    PAGE.replace("{{website_url}}", WEBSITE_URL)
}

/// A site stylesheet by exact filename.
pub fn stylesheet(name: &str) -> Option<Response> {
    let source = match name {
        "tokens.css" => TOKENS_CSS,
        "global.css" => GLOBAL_CSS,
        "components.css" => COMPONENTS_CSS,
        "fonts.css" => FONTS_CSS,
        "landing.css" => LANDING_CSS,
        _ => return None,
    };
    Some(static_response(
        "text/css; charset=utf-8",
        CACHE_PUBLIC,
        Body::from(source),
    ))
}

/// A font file by exact filename.
pub fn font(name: &str) -> Option<Response> {
    let source = match name {
        "inter-latin.woff2" => INTER_WOFF2,
        "space-grotesk-latin.woff2" => SPACE_GROTESK_WOFF2,
        _ => return None,
    };
    Some(static_response(
        "font/woff2",
        CACHE_PUBLIC,
        Body::from(source),
    ))
}

/// The favicon, shared with the homepage and the docs site.
pub fn favicon() -> Response {
    static_response("image/svg+xml", CACHE_PUBLIC, Body::from(FAVICON))
}

/// Answer for a static path that names no embedded asset.
pub fn not_found() -> Response {
    let mut response = static_response(
        "text/plain; charset=utf-8",
        CACHE_PAGE,
        Body::from("Not Found\n"),
    );
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}

fn static_response(
    content_type: &'static str,
    cache_control: &'static str,
    body: Body,
) -> Response {
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(cache_control),
    );
    with_security_headers(response)
}

/// Apply the hardening header block every static response carries.
///
/// The page is static and script-free, hence `default-src 'none'`. No header
/// is inherited by API responses.
fn with_security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_XSS_PROTECTION, HeaderValue::from_static("1"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_links_the_site_and_every_stylesheet() {
        let page = page_body();
        assert!(page.contains(WEBSITE_URL));
        assert!(!page.contains("{{"));
        for name in CSS_FILES {
            assert!(
                page.contains(&format!(r#"href="/css/{name}""#)),
                "the page must link {name}"
            );
        }
        assert!(page.contains(r#"href="/favicon.svg""#));
    }

    #[test]
    fn every_declared_file_resolves_and_nothing_else_does() {
        for name in CSS_FILES {
            assert!(stylesheet(name).is_some(), "{name} must be embedded");
        }
        assert!(stylesheet("nope.css").is_none());
        for name in FONT_FILES {
            assert!(font(name).is_some(), "{name} must be embedded");
        }
        assert!(font("nope.woff2").is_none());
    }
}
