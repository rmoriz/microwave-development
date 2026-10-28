use crate::mcp::context::McpContext;
use crate::{ApiError, AppState};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use microwave_store::ActorType;
use subtle::ConstantTimeEq;

/// Resolve the organization, bearer token, and principal for an MCP request.
///
/// Failures are returned as a ready-to-send response so the caller never has to
/// merge partially authenticated state.
// Returns the response directly, matching the handler's error path; boxing it
// would force every caller to unwrap one more layer.
#[allow(clippy::result_large_err)]
pub async fn authenticate(
    state: &AppState,
    organization: &str,
    headers: &HeaderMap,
) -> Result<McpContext, Response> {
    let Some(org) = state.store.find_org(organization).await.map_err(internal)? else {
        return Err(ApiError::not_found("not found").into_response());
    };

    let Some(entry) = state.config.mcp.organizations.get(&org.name) else {
        return Err(ApiError::not_found("not found").into_response());
    };

    let configured = match entry.token.as_deref().and_then(non_empty) {
        Some(token) => Some(token.to_string()),
        None => state
            .store
            .get_secret("mcp", &org.name)
            .await
            .map_err(internal)?
            .as_deref()
            .and_then(non_empty)
            .map(ToString::to_string),
    };
    let Some(expected) = configured else {
        tracing::warn!(
            organization = %org.name,
            "mcp endpoint is configured without a token"
        );
        return Err(unauthorized());
    };

    let Some(provided) = bearer_token(headers) else {
        return Err(unauthorized());
    };
    if !bool::from(expected.as_bytes().ct_eq(provided.as_bytes())) {
        return Err(unauthorized());
    }

    let name = entry
        .principal
        .as_deref()
        .and_then(non_empty)
        .unwrap_or(&state.config.auth.superuser_name);
    let Some(principal) = state
        .store
        .find_principals(Some(&org.id), name)
        .await
        .map_err(internal)?
        .into_iter()
        .find(|principal| principal.actor_type == ActorType::User)
    else {
        tracing::error!(
            organization = %org.name,
            principal = %name,
            "mcp principal does not exist"
        );
        return Err(ApiError::internal().into_response());
    };

    let is_superuser = name == state.config.auth.superuser_name;
    if !is_superuser {
        match state
            .store
            .is_user_in_org(&org.id, &principal.actor_id)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                tracing::error!(
                    organization = %org.name,
                    principal = %name,
                    "mcp principal is not a member of the organization"
                );
                return Err(ApiError::internal().into_response());
            }
            Err(error) => {
                tracing::error!(
                    organization = %org.name,
                    principal = %name,
                    %error,
                    "failed to check mcp principal membership"
                );
                return Err(ApiError::internal().into_response());
            }
        }
    }

    Ok(McpContext {
        state: state.clone(),
        org,
        principal,
        is_superuser,
        writes: entry.writes,
        max_results: state.config.mcp.max_results,
        max_result_bytes: state.config.mcp.max_result_bytes,
        max_upload_bytes: state.config.mcp.max_upload_bytes,
    })
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

/// Extract a non-empty bearer token using a case-insensitive scheme match.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    non_empty(token)
}

fn unauthorized() -> Response {
    let mut response = ApiError::unauthorized_with("Failed to authenticate").into_response();
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn internal(error: microwave_store::StoreError) -> Response {
    tracing::error!(%error, "mcp authentication store error");
    ApiError::internal().into_response()
}
