//! Forwarding and local storage for `POST /organizations/:org/data-collector`.
//!
//! The route is enabled when `data_collector['root_url']` is configured or when
//! `data_collector.store` keeps payloads locally. A request is accepted either
//! with the configured shared `x-data-collector-token` or with normal Chef
//! signed authentication. `run_converge` payloads can be stored on this server
//! and are, when forwarding is enabled, also sent verbatim to the configured
//! Automate/data collector URL with a bounded timeout.

use crate::{ApiError, AppState};
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use std::time::Duration;
use subtle::ConstantTimeEq;

/// Upper bound for an upstream data-collector response. Automate returns short
/// acknowledgements; this prevents a misbehaving upstream from exhausting the
/// server's memory.
const MAX_RESPONSE_BYTES: usize = 1_048_576;

/// The token used both to authenticate inbound requests and to authenticate
/// the forwarded request. Configuration wins, then the `data_collector.token`
/// secret populated by `microwave-ctl set-secret data_collector token`.
pub(crate) async fn configured_token(state: &AppState) -> Option<String> {
    if let Some(token) = state
        .config
        .data_collector
        .token
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        return Some(token.to_string());
    }
    state
        .store
        .get_secret("data_collector", "token")
        .await
        .ok()
        .flatten()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
}

/// Return true when the request carries the configured shared token. The
/// comparison is constant-time because the token is a bearer credential.
pub(crate) async fn has_valid_token(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(expected) = configured_token(state).await else {
        return false;
    };
    let Some(provided) = headers
        .get("x-data-collector-token")
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    expected.as_bytes().ct_eq(provided.as_bytes()).into()
}

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/organizations/{organization}/data-collector",
        post(forward),
    )
}

async fn forward(
    State(state): State<AppState>,
    Path(organization): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let collector = &state.config.data_collector;
    let storing = collector.stores_locally();
    let forwarding = collector.is_configured() && collector.forwards();

    if storing {
        let org = match state.store.find_org(&organization).await {
            Ok(Some(org)) => org,
            Ok(None) => return ApiError::not_found("not found").into_response(),
            Err(error) => {
                tracing::warn!(%error, org = %organization, "failed to resolve data collector organization");
                return ApiError::internal().into_response();
            }
        };

        match crate::report_ingest::prepare_node_run(&body) {
            Ok(Some(run)) => {
                let new_run = microwave_store::NewNodeRun {
                    node_name: &run.node_name,
                    run_id: &run.run_id,
                    status: &run.status,
                    start_time: Some(&run.start_time),
                    end_time: run.end_time.as_deref(),
                    resource_count: run.resource_count,
                    updated_resource_count: run.updated_resource_count,
                    error_count: run.error_count,
                    report: &run.report,
                };
                if let Err(error) = state
                    .store
                    .store_node_run(&org.id, new_run, collector.keep_runs)
                    .await
                {
                    tracing::warn!(%error, org = %org.name, "failed to store node run");
                    return ApiError::internal().into_response();
                }
            }
            Ok(None) => {}
            Err(error) => return error.into_response(),
        }
    }

    if forwarding {
        forward_upstream(&state, headers, body).await
    } else {
        (StatusCode::OK, axum::Json(serde_json::json!({}))).into_response()
    }
}

async fn forward_upstream(state: &AppState, headers: HeaderMap, body: Bytes) -> Response {
    let Some(root_url) = state
        .config
        .data_collector
        .root_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        return ApiError::not_found("not found").into_response();
    };

    let timeout = Duration::from_millis(state.config.data_collector.timeout.max(1));
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("application/json"));
    let mut request = state
        .http_client
        .post(root_url)
        .timeout(timeout)
        .header(header::CONTENT_TYPE, content_type)
        // Upstream nginx always sends the auth version to the external
        // collector and replaces any client-supplied token header with the
        // server-side one when configured.
        .header("x-data-collector-auth", "version=1.0")
        .body(body);

    if let Some(token) = configured_token(state).await {
        request = request.header("x-data-collector-token", token);
    }

    let mut upstream = match request.send().await {
        Ok(response) => response,
        Err(error) if error.is_timeout() => {
            tracing::warn!(%error, url = root_url, "data collector request timed out");
            return ApiError::gateway_timeout("data collector request timed out").into_response();
        }
        Err(error) => {
            tracing::warn!(%error, url = root_url, "data collector request failed");
            return ApiError::bad_gateway("data collector request failed").into_response();
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
                    tracing::warn!(url = root_url, "data collector response exceeded size cap");
                    return ApiError::bad_gateway("data collector response too large")
                        .into_response();
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(error) if error.is_timeout() => {
                tracing::warn!(%error, url = root_url, "data collector response timed out");
                return ApiError::gateway_timeout("data collector response timed out")
                    .into_response();
            }
            Err(error) => {
                tracing::warn!(%error, url = root_url, "failed to read data collector response");
                return ApiError::bad_gateway("failed to read data collector response")
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
