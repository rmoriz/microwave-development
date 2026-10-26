use crate::error::OAuthError;
use axum::extract::Request;
use axum::http::header;
use axum::http::uri::Authority;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::str::FromStr;

/// Reject cross-site form submissions that could ride an existing session
/// cookie. JSON/API requests and non-cookie OAuth endpoints are unchanged.
pub async fn require_same_origin(request: Request, next: Next) -> Response {
    if is_cookie_form_write(
        request.method().as_str(),
        request.uri().path(),
        request.headers(),
    ) && !same_origin(request.headers())
    {
        return OAuthError::forbidden("cross-origin form submission rejected").into_response();
    }
    next.run(request).await
}

fn is_cookie_form_write(method: &str, path: &str, headers: &axum::http::HeaderMap) -> bool {
    if !matches!(method, "POST" | "PUT" | "PATCH" | "DELETE") {
        return false;
    }
    if path == "/id/oauth/token" || path == "/id/oauth/revoke" || path.starts_with("/id/v1/") {
        return false;
    }
    let form = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.starts_with("application/x-www-form-urlencoded")
                || value.starts_with("multipart/form-data")
        });
    form && (path == "/id/sessions"
        || path == "/id/auth/chef/callback"
        || path == "/id/signout"
        || path.starts_with("/id/oauth/authorize")
        || path.starts_with("/id/oauth/applications")
        || path.starts_with("/id/profile"))
}

fn same_origin(headers: &axum::http::HeaderMap) -> bool {
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Authority::from_str(value).ok())
    else {
        return false;
    };
    let source = headers
        .get(header::ORIGIN)
        .or_else(|| headers.get(header::REFERER))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<axum::http::Uri>().ok());
    let Some(authority) = source.as_ref().and_then(|uri| uri.authority()) else {
        return false;
    };
    authority.host().eq_ignore_ascii_case(host.host()) && authority.port_u16() == host.port_u16()
}
