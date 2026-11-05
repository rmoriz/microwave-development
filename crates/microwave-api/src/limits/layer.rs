//! Admission middleware: global concurrency and per-source rate.

use super::source::client_ip;
use super::{Rejection, Scope};
use crate::middleware::{is_static_asset_path, serves_landing_page};
use crate::{AppState, Metrics};
use axum::extract::{Request, State};
use axum::http::{header::RETRY_AFTER, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Marker that keeps the gate from running twice when the router re-enters
/// itself for the default organization or a trailing slash.
#[derive(Clone, Copy)]
pub struct AdmissionApplied;

/// Paths that stay reachable when every permit is taken.
pub fn is_exempt(request: &Request) -> bool {
    let path = request.uri().path();
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

fn log_rejection(request: &Request, rejection: Rejection) {
    let request_id = request
        .extensions()
        .get::<crate::middleware::RequestId>()
        .map(|id| id.0.as_str())
        .unwrap_or("");
    tracing::warn!(
        request_id = %request_id,
        method = %request.method(),
        path = %request.uri().path(),
        scope = rejection.scope.as_str(),
        org = "",
        principal = "",
        retry_after = rejection.retry_after,
        "request rejected by limit"
    );
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
            log_rejection(&request, rejection);
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
                log_rejection(&request, rejection);
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
