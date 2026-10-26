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

    let upstream_url = match upstream_url(base, &owner, profile.as_deref(), uri.query()) {
        Ok(url) => url,
        Err(error) => return error.into_response(),
    };

    if let Err(error) = authorize_org(&state, &auth, &organization).await {
        return error.into_response();
    }

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

    if auth.principal.actor_type == ActorType::User && !auth.is_superuser {
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
    if let Err(error) = state.config.compliance_proxy.validate() {
        tracing::warn!(
            %error,
            header = configured,
            "invalid compliance_proxy configuration; using the default token header"
        );
        return HeaderName::from_static(DEFAULT_TOKEN_HEADER);
    }
    HeaderName::from_bytes(configured.as_bytes()).unwrap_or_else(|error| {
        tracing::warn!(
            %error,
            header = configured,
            "invalid compliance_proxy.token_header; using the default"
        );
        HeaderName::from_static(DEFAULT_TOKEN_HEADER)
    })
}

fn upstream_url(
    base: &str,
    owner: &str,
    profile: Option<&str>,
    query: Option<&str>,
) -> Result<String, ApiError> {
    validate_segment(owner, "owner")?;

    let mut parsed = reqwest::Url::parse(base).map_err(|error| {
        tracing::warn!(%error, "invalid compliance_proxy.url");
        ApiError::internal()
    })?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        tracing::error!("compliance_proxy.url must be an http(s) URL with a host");
        return Err(ApiError::internal());
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    let base_path = parsed.path().trim_end_matches('/').to_string();

    let mut profile_segments = Vec::new();
    if let Some(profile) = profile.filter(|profile| !profile.is_empty()) {
        // Upstream nginx accepts a single trailing slash. Normalize it away,
        // but still reject empty internal segments (double slashes) below.
        let profile = profile.strip_suffix('/').unwrap_or(profile);
        if !profile.is_empty() {
            for segment in profile.split('/') {
                validate_segment(segment, "profile")?;
                profile_segments.push(segment);
            }
        }
    }

    {
        let mut segments = parsed
            .path_segments_mut()
            .map_err(|_| ApiError::bad_request("invalid compliance proxy base URL"))?;
        segments.pop_if_empty();
        segments.push("compliance");
        segments.push("profiles");
        segments.push(owner);
        for segment in &profile_segments {
            segments.push(segment);
        }
    }
    if let Some(query) = query.filter(|query| !query.is_empty()) {
        parsed.set_query(Some(query));
    }

    // Defense in depth: even if a URL parser quirk slips through, never send
    // a request outside the intended profile prefix.
    let expected_prefix = format!("{base_path}/compliance/profiles/");
    if !parsed.path().starts_with(&expected_prefix) {
        tracing::warn!(
            path = parsed.path(),
            expected_prefix,
            "compliance proxy URL escaped the profile prefix"
        );
        return Err(ApiError::bad_request("invalid compliance profile path"));
    }

    Ok(parsed.to_string())
}

/// Reject path segments that a URL parser could normalize into a different
/// upstream path. Slashes and backslashes are rejected per-segment; profile
/// values are split on `/` before this check.
fn validate_segment(segment: &str, kind: &str) -> Result<(), ApiError> {
    if segment.is_empty()
        || segment == "."
        || segment == ".."
        || segment.contains('/')
        || segment.contains('\\')
        || segment.chars().any(char::is_control)
    {
        return Err(ApiError::bad_request(format!(
            "invalid compliance {kind} segment"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_both_upstream_spellings_to_the_profile_path() {
        assert_eq!(
            upstream_url("https://automate.example.com", "foobar", None, None).unwrap(),
            "https://automate.example.com/compliance/profiles/foobar"
        );
        assert_eq!(
            upstream_url(
                "https://automate.example.com/",
                "foobar",
                Some("testprofile"),
                None
            )
            .unwrap(),
            "https://automate.example.com/compliance/profiles/foobar/testprofile"
        );
        assert_eq!(
            upstream_url(
                "https://automate.example.com",
                "foobar",
                Some("base/linux"),
                Some("refresh=true")
            )
            .unwrap(),
            "https://automate.example.com/compliance/profiles/foobar/base/linux?refresh=true"
        );
    }

    #[test]
    fn preserves_a_base_path_and_percent_encodes_segments() {
        assert_eq!(
            upstream_url(
                "https://automate.example.com/api",
                "foo bar",
                Some("base/linux"),
                None
            )
            .unwrap(),
            "https://automate.example.com/api/compliance/profiles/foo%20bar/base/linux"
        );
    }

    #[test]
    fn rejects_dot_segments_in_owner_and_profile() {
        for owner in [".", "..", "foo/../bar", "foo\\..\\bar"] {
            assert!(
                upstream_url("https://automate.example.com", owner, None, None).is_err(),
                "accepted owner {owner}"
            );
        }
        for profile in ["..", "../admin", "base/../../admin", "base//linux"] {
            assert!(
                upstream_url(
                    "https://automate.example.com",
                    "foobar",
                    Some(profile),
                    None
                )
                .is_err(),
                "accepted profile {profile}"
            );
        }
    }

    #[test]
    fn normalizes_one_profile_trailing_slash_and_rejects_empty_segments() {
        assert_eq!(
            upstream_url(
                "https://automate.example.com",
                "foobar",
                Some("testprofile/"),
                None
            )
            .unwrap(),
            "https://automate.example.com/compliance/profiles/foobar/testprofile"
        );
        assert_eq!(
            upstream_url("https://automate.example.com", "foobar", Some("/"), None).unwrap(),
            "https://automate.example.com/compliance/profiles/foobar"
        );
        for profile in ["testprofile//", "base//linux"] {
            assert!(
                upstream_url(
                    "https://automate.example.com",
                    "foobar",
                    Some(profile),
                    None
                )
                .is_err(),
                "accepted profile with empty segment {profile}"
            );
        }
    }

    #[test]
    fn keeps_the_intended_prefix_even_with_encoded_slashes() {
        assert_eq!(
            upstream_url("https://automate.example.com", "foo/bar", None, None)
                .expect_err("slash in owner must be rejected")
                .status,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            upstream_url(
                "https://automate.example.com",
                "foobar",
                Some("a/../admin"),
                None
            )
            .expect_err("traversal in profile must be rejected")
            .status,
            StatusCode::BAD_REQUEST
        );
    }
}
