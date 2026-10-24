use crate::error::OAuthError;
use crate::state::OAuthState;
use crate::util::{bearer_token, bearer_user, user_json};
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};

pub async fn status() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "erchef": { "status": "reachable" },
        "postgres": { "status": "reachable" }
    }))
}

pub async fn me(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Json<Value>, OAuthError> {
    let user = require_bearer_user(&state, &headers)?;
    Ok(Json(user_json(&state, &user)))
}

pub async fn organizations(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Json<Value>, OAuthError> {
    let user = require_bearer_user(&state, &headers)?;
    let organizations = state.store.list_user_orgs(&user.id)?;
    let organizations = organizations
        .into_iter()
        .map(|organization| {
            json!({
                "name": organization.name,
                "guid": organization.id
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(Value::Array(organizations)))
}

pub async fn user(
    State(state): State<OAuthState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, OAuthError> {
    require_bearer_user(&state, &headers)?;
    let user = state
        .store
        .find_user(&id)?
        .or(state.store.find_user_by_id(&id)?)
        .ok_or_else(|| OAuthError::not_found(format!("user {id}")))?;
    Ok(Json(json!({
        "id": user.username,
        "username": user.username,
        "first_name": Value::Null,
        "middle_name": Value::Null,
        "last_name": Value::Null,
        "display_name": user.username
    })))
}

fn require_bearer_user(
    state: &OAuthState,
    headers: &HeaderMap,
) -> Result<microwave_store::User, OAuthError> {
    let token = bearer_token(headers)
        .ok_or_else(|| OAuthError::unauthorized("bearer token is required"))?;
    bearer_user(state, &token)?.ok_or_else(|| OAuthError::unauthorized("invalid bearer token"))
}
