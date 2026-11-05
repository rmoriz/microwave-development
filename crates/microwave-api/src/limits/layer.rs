//! Admission middleware: global concurrency and per-source rate.

use super::source::client_ip;
use super::{LimitPermit, Rejection, Scope};
use crate::bookshelf::{authorized, TokenQuery};
use crate::middleware::{is_static_asset_path, serves_landing_page};
use crate::{AppState, Metrics};
use axum::extract::{Query, Request, State};
use axum::http::{header::RETRY_AFTER, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Marker that keeps the gate from running twice when the router re-enters
/// itself for the default organization or a trailing slash.
#[derive(Clone, Copy)]
pub struct AdmissionApplied;

/// Paths that stay reachable when every permit is taken.
///
/// The lookup strips one trailing slash because this layer runs outside the
/// normalizer that later rewrites `/version/` to `/version`.
fn is_exempt(request: &Request) -> bool {
    let path = request.uri().path();
    let path = path
        .strip_suffix('/')
        .filter(|path| !path.is_empty())
        .unwrap_or(path);
    matches!(
        path,
        "/_status" | "/_stats" | "/version" | "/server_api_version" | "/license"
    ) || is_static_asset_path(path)
        || (path == "/" && serves_landing_page(path, request.headers()))
}

pub fn overloaded(retry_after: u64) -> Response {
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(json!({ "error": ["Server is overloaded. Retry later."] })),
    )
        .into_response();
    let seconds = retry_after.max(1).to_string();
    response.headers_mut().insert(
        RETRY_AFTER,
        HeaderValue::from_str(&seconds).unwrap_or(HeaderValue::from_static("1")),
    );
    response
}

/// Count a rejection and answer `503`.
pub fn rejected(rejection: Rejection, metrics: &Metrics, org: &str) -> Response {
    metrics.record_limit(rejection.scope.as_str(), org, "rejected");
    overloaded(rejection.retry_after)
}

/// The request coordinates a rejection log line carries. Owned pieces of the
/// request, so a `&Request` is not held across the await (it is not `Sync`).
pub struct RejectionLog<'a> {
    pub request_id: &'a str,
    pub method: &'a axum::http::Method,
    pub path: &'a str,
}

impl<'a> RejectionLog<'a> {
    pub fn of(request: &'a Request) -> Self {
        Self {
            request_id: request
                .extensions()
                .get::<crate::middleware::RequestId>()
                .map(|id| id.0.as_str())
                .unwrap_or(""),
            method: request.method(),
            path: request.uri().path(),
        }
    }
}

/// One rejection log line, joinable with the access log through the request
/// id.
fn log_rejection(log: &RejectionLog<'_>, org: &str, principal: &str, rejection: Rejection) {
    tracing::warn!(
        request_id = %log.request_id,
        method = %log.method,
        path = %log.path,
        scope = rejection.scope.as_str(),
        org,
        principal,
        retry_after = rejection.retry_after,
        "request rejected by limit"
    );
}

/// Org and principal limits, applied after authentication. Both permits live
/// until the request is done.
#[allow(clippy::result_large_err)]
pub async fn check_org_principal(
    state: &AppState,
    log: &RejectionLog<'_>,
    org: Option<&microwave_store::Org>,
    principal: &microwave_store::Principal,
) -> Result<(LimitPermit, LimitPermit), Response> {
    let Some(org) = org else {
        return Ok((LimitPermit::empty(), LimitPermit::empty()));
    };
    let org_permit = match state.limits.org(org).await {
        Ok(permit) => {
            state
                .metrics
                .record_limit(Scope::Org.as_str(), &org.name, "allowed");
            permit
        }
        Err(rejection) => {
            log_rejection(log, &org.name, &principal.name, rejection);
            return Err(rejected(rejection, &state.metrics, &org.name));
        }
    };
    let principal_permit = match state.limits.principal(org, principal).await {
        Ok(permit) => {
            state
                .metrics
                .record_limit(Scope::Principal.as_str(), &org.name, "allowed");
            permit
        }
        Err(rejection) => {
            drop(org_permit);
            log_rejection(log, &org.name, &principal.name, rejection);
            return Err(rejected(rejection, &state.metrics, &org.name));
        }
    };
    Ok((org_permit, principal_permit))
}

/// `/bookshelf/organizations/:org/checksums/:checksum` parts.
fn bookshelf_target(path: &str) -> Option<(&str, &str)> {
    let mut parts = path.trim_matches('/').split('/');
    if parts.next() != Some("bookshelf") || parts.next() != Some("organizations") {
        return None;
    }
    let organization = parts.next().filter(|part| !part.is_empty())?;
    if parts.next() != Some("checksums") {
        return None;
    }
    let checksum = parts.next().filter(|part| !part.is_empty())?;
    Some((organization, checksum))
}

/// Count bookshelf uploads and downloads against the org quota.
///
/// Bookshelf authenticates with a query token rather than a signature, so this
/// gate sits in front of the body extractor: a rejected upload never buffers
/// its body. Downloads count as well because the org share is about transfer
/// pressure, not only storage writes. The HMAC check also runs in the handler;
/// the second pass is deliberate so an unauthenticated request consumes no
/// token and the gate does not have to duplicate the handler's error shapes.
pub async fn bookshelf_org_gate(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if !state.limits.enabled() {
        return next.run(request).await;
    }
    if !matches!(
        *request.method(),
        axum::http::Method::GET
            | axum::http::Method::HEAD
            | axum::http::Method::PUT
            | axum::http::Method::DELETE
    ) {
        // Every other method has no handler and keeps the router's `405`.
        return next.run(request).await;
    }
    let method = if request.method() == axum::http::Method::HEAD {
        "GET".to_string()
    } else {
        request.method().as_str().to_string()
    };
    let Some((organization, checksum)) = bookshelf_target(request.uri().path()) else {
        return next.run(request).await;
    };
    let query = match Query::<TokenQuery>::try_from_uri(request.uri()) {
        Ok(Query(query)) => query,
        Err(response) => return response.into_response(),
    };
    if let Err(error) = authorized(&state, &method, organization, checksum, &query) {
        return error.into_response();
    }

    // One indexed lookup keeps the quota override and the metric label on the
    // organization name; the path only carries the ID. A missing row (blobs
    // outliving their org) falls back to the ID.
    let name = state
        .store
        .find_org_by_id(organization)
        .await
        .ok()
        .flatten()
        .map(|org| org.name);
    let label = name.as_deref().unwrap_or(organization);
    let permit = match state.limits.org_by_key(organization, name.as_deref()).await {
        Ok(permit) => {
            state
                .metrics
                .record_limit(Scope::Org.as_str(), label, "allowed");
            permit
        }
        Err(rejection) => {
            log_rejection(&RejectionLog::of(&request), label, "", rejection);
            return rejected(rejection, &state.metrics, label);
        }
    };
    let response = next.run(request).await;
    drop(permit);
    response
}

/// Global admission and the pre-auth source bucket.
pub async fn admission(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if request.extensions().get::<AdmissionApplied>().is_some() {
        return next.run(request).await;
    }
    if !state.limits.enabled() || is_exempt(&request) {
        return next.run(request).await;
    }
    request.extensions_mut().insert(AdmissionApplied);

    let global = match state.limits.global().await {
        Ok(permit) => permit,
        Err(rejection) => {
            log_rejection(&RejectionLog::of(&request), "", "", rejection);
            return rejected(rejection, &state.metrics, "");
        }
    };
    state
        .metrics
        .record_limit(Scope::Global.as_str(), "", "allowed");

    let client = client_ip(&request, &state.config.server.trusted_proxies);
    let source = match client {
        Some(address) => match state.limits.source(address).await {
            Ok(permit) => {
                state
                    .metrics
                    .record_limit(Scope::Source.as_str(), "", "allowed");
                Some(permit)
            }
            Err(rejection) => {
                log_rejection(&RejectionLog::of(&request), "", "", rejection);
                return rejected(rejection, &state.metrics, "");
            }
        },
        None => None,
    };

    let response = next.run(request).await;
    drop(source);
    drop(global);
    response
}
