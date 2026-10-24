use crate::{ApiError, AppState, AuthContext};
use axum::http::HeaderMap;
use microwave_store::{Org, Permission};
use serde_json::Value;

pub fn organization(auth: &AuthContext) -> Result<&Org, ApiError> {
    auth.org
        .as_ref()
        .ok_or_else(|| ApiError::bad_request("organization is required"))
}

pub fn base_url(headers: &HeaderMap, state: &AppState) -> String {
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or(state.config.server.bind.as_str());
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("http");
    format!("{proto}://{host}")
}

pub fn container_authz(
    state: &AppState,
    org: &Org,
    container_name: &str,
) -> Result<String, ApiError> {
    Ok(state
        .store
        .find_container(&org.id, container_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("container {container_name}")))?
        .authz_id)
}

pub fn require_permission(
    state: &AppState,
    auth: &AuthContext,
    target_authz_id: &str,
    permission: Permission,
) -> Result<(), ApiError> {
    if is_superuser(auth) {
        return Ok(());
    }
    if state
        .store
        .check_permission(&auth.principal.authz_id, target_authz_id, permission)
        .map_err(ApiError::from)?
    {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}

pub fn is_superuser(auth: &AuthContext) -> bool {
    auth.principal.name == "pivotal"
}

pub fn require_superuser(auth: &AuthContext) -> Result<(), ApiError> {
    if is_superuser(auth) {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}

/// Validate a `public_key` member. A missing or JSON null value means "no key
/// was supplied"; every other value must be a valid PEM public key.
pub fn parse_public_key(body: &Value) -> Result<Option<String>, ApiError> {
    match body.get("public_key") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            if value.trim().is_empty() {
                return Err(ApiError::bad_request("Field 'public_key' is invalid"));
            }
            microwave_crypto::parse_public_key_pem(value.trim())
                .map_err(|_| ApiError::bad_request("Field 'public_key' is invalid"))?;
            Ok(Some(value.clone()))
        }
        Some(_) => Err(ApiError::bad_request("Field 'public_key' is invalid")),
    }
}
