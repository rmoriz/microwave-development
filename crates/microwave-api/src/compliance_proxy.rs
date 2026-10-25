//! Compliance profile proxy compatibility routes.
//!
//! Upstream nginx exposes the Automate compliance profile API under two
//! organization-scoped spellings (see `chef_http_lb_common`):
//!
//! - `GET /organizations/:org/owners/:owner/compliance[/profile]`
//! - `GET /compliance/organizations/:org/owners/:owner/compliance[/profile]`
//!
//! Both are rewritten to `/compliance/profiles/:owner[/profile]` and forwarded
//! with the server-side data collector token. The routes exist only when
//! `compliance_proxy.url` is configured; otherwise the normal fallback returns
//! `404`. Status, body, and content type from the upstream are preserved.
//! Client-supplied Chef authorization headers are never forwarded.

use crate::{ApiError, AppState, AuthContext};
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderName, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Router};
use microwave_store::ActorType;
use std::time::Duration;

/// Automate profile responses are small JSON documents. Bound the relay so a
/// misbehaving upstream cannot exhaust server memory.
const MAX_RESPONSE_BYTES: usize = 1_048_576;
const DEFAULT_TOKEN_HEADER: &str = "x-data-collector-token";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/owners/{owner}/compliance",
            get(forward_base),
        )
        .route(
            "/organizations/{organization}/owners/{owner}/compliance/{*profile}",
            get(forward_profile),
        )
        .route(
            "/compliance/organizations/{organization}/owners/{owner}/compliance",
            get(forward_base),
        )
        .route(
            "/compliance/organizations/{organization}/owners/{owner}/compliance/{*profile}",
            get(forward_profile),
        )
}

async fn forward_base(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((organization, owner)): Path<(String, String)>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    forward(state, auth, organization, owner, None, headers, uri).await
}

async fn forward_profile(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((organization, owner, profile)): Path<(String, String, String)>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    forward(
        state,
        auth,
        organization,
        owner,
        Some(profile),
        headers,
        uri,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn forward(
    state: AppState,
    auth: AuthContext,
    organization: String,
    owner: String,
    profile: Option<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let Some(base) = state
        .config
        .compliance_proxy
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        return ApiError::not_found("not found").into_response();
    };

    if let Err(error) = authorize_org(&state, &auth, &organization).await {
        return error.into_response();
    }

    let upstream_url = upstream_url(base, &owner, profile.as_deref(), uri.query());
    let timeout = Duration::from_millis(state.config.compliance_proxy.timeout.max(1));
    let mut request = state
        .compliance_client
        .get(&upstream_url)
        .timeout(timeout)
        // Upstream nginx always sets the data-collector auth version and
        // replaces any client-supplied token header with the server value.
        .header("x-data-collector-auth", "version=1.0");

    for name in [header::ACCEPT, header::CONTENT_TYPE, header::USER_AGENT] {
        if let Some(value) = headers.get(&name) {
            request = request.header(name, value.clone());
        }
    }
    if let Some(request_id) = headers.get("x-request-id") {
        request = request.header("x-request-id", request_id.clone());
    }

    if let Some(token) = configured_token(&state).await {
        let header_name = token_header(&state);
        request = request.header(header_name, token);
    }

    let mut upstream = match request.send().await {
        Ok(response) => response,
        Err(error) if error.is_timeout() => {
            tracing::warn!(%error, url = upstream_url, "compliance proxy request timed out");
            return ApiError::gateway_timeout("compliance proxy request timed out").into_response();
        }
        Err(error) => {
            tracing::warn!(%error, url = upstream_url, "compliance proxy request failed");
            return ApiError::bad_gateway("compliance proxy request failed").into_response();
        }
    };

    let status =
        StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
    let mut body = Vec::new();
    loop {
        match upstream.chunk().await {
            Ok(Some(chunk)) => {
                if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                    tracing::warn!(
                        url = upstream_url,
                        "compliance proxy response exceeded size cap"
                    );
                    return ApiError::bad_gateway("compliance proxy response too large")
                        .into_response();
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) if error.is_timeout() => {
                tracing::warn!(%error, url = upstream_url, "compliance proxy response timed out");
                return ApiError::gateway_timeout("compliance proxy response timed out")
                    .into_response();
            }
            Err(error) => {
                tracing::warn!(%error, url = upstream_url, "failed to read compliance proxy response");
                return ApiError::bad_gateway("failed to read compliance proxy response")
                    .into_response();
            }
        }
    }

    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    }
    response
}

/// `authorize` the signed requestor for the organization named in the path.
/// The normal API middleware already checks membership for
/// `/organizations/:org/...`, but this also covers the legacy
/// `/compliance/organizations/:org/...` spelling, which has no organization in
/// the prefix that the middleware recognizes.
async fn authorize_org(
    state: &AppState,
    auth: &AuthContext,
    organization: &str,
) -> Result<(), ApiError> {
    let org = state
        .store
        .find_org(organization)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("organization {organization} not found")))?;

    if auth.principal.actor_type == ActorType::User && auth.principal.name != "pivotal" {
        let member = state
            .store
            .is_user_in_org(&org.id, &auth.principal.actor_id)
            .await
            .map_err(ApiError::from)?;
        if !member {
            return Err(ApiError::forbidden_with(format!(
                "'{}' not associated with organization '{}'",
                auth.principal.name, org.name
            )));
        }
    }

    if let Some(principal_org) = auth.principal.org_id.as_deref() {
        if principal_org != org.id.as_str() {
            return Err(ApiError::forbidden_with(format!(
                "'{}' not associated with organization '{}'",
                auth.principal.name, org.name
            )));
        }
    }
    if let Some(auth_org) = &auth.org {
        if auth_org.id != org.id {
            return Err(ApiError::forbidden_with(format!(
                "'{}' not associated with organization '{}'",
                auth.principal.name, org.name
            )));
        }
    }

    Ok(())
}

/// Resolve the service token. Configuration wins; otherwise fall back to the
/// shared data collector token (configuration or secret), matching upstream's
/// single `$data_collector_token` nginx variable.
async fn configured_token(state: &AppState) -> Option<String> {
    if let Some(token) = state
        .config
        .compliance_proxy
        .token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        return Some(token.to_string());
    }
    crate::data_collector::configured_token(state).await
}

fn token_header(state: &AppState) -> HeaderName {
    let configured = state
        .config
        .compliance_proxy
        .token_header
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(DEFAULT_TOKEN_HEADER);
    HeaderName::from_bytes(configured.as_bytes()).unwrap_or_else(|error| {
        tracing::warn!(
            %error,
            header = configured,
            "invalid compliance_proxy.token_header; using the default"
        );
        HeaderName::from_static(DEFAULT_TOKEN_HEADER)
    })
}

fn upstream_url(base: &str, owner: &str, profile: Option<&str>, query: Option<&str>) -> String {
    let mut url = format!(
        "{}/compliance/profiles/{}",
        base.trim_end_matches('/'),
        encode_segment(owner)
    );
    if let Some(profile) = profile.filter(|profile| !profile.is_empty()) {
        let encoded = profile
            .split('/')
            .map(encode_segment)
            .collect::<Vec<_>>()
            .join("/");
        url.push('/');
        url.push_str(&encoded);
    }
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        url.push('?');
        url.push_str(query);
    }
    url
}

/// Percent-encode a single path segment. Owners and profiles originate from
/// the request path, so they must not be able to inject a different upstream
/// path or query string.
fn encode_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_both_upstream_spellings_to_the_profile_path() {
        assert_eq!(
            upstream_url("https://automate.example.com", "foobar", None, None),
            "https://automate.example.com/compliance/profiles/foobar"
        );
        assert_eq!(
            upstream_url(
                "https://automate.example.com/",
                "foobar",
                Some("testprofile"),
                None
            ),
            "https://automate.example.com/compliance/profiles/foobar/testprofile"
        );
        assert_eq!(
            upstream_url(
                "https://automate.example.com",
                "foobar",
                Some("base/linux"),
                Some("refresh=true")
            ),
            "https://automate.example.com/compliance/profiles/foobar/base/linux?refresh=true"
        );
    }

    #[test]
    fn encodes_path_segments_instead_of_passing_them_through() {
        assert_eq!(encode_segment("a/b?c"), "a%2Fb%3Fc");
        assert_eq!(encode_segment("foo bar"), "foo%20bar");
    }
}
