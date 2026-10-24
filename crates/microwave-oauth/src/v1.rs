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
    let user = require_bearer_user(&state, &headers).await?;
    Ok(Json(user_json(&state, &user).await))
}

pub async fn organizations(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Json<Value>, OAuthError> {
    let user = require_bearer_user(&state, &headers).await?;
    let organizations = state.store.list_user_orgs(&user.id).await?;
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
    require_bearer_user(&state, &headers).await?;
    let user = state
        .store
        .find_user(&id)
        .await?
        .or(state.store.find_user_by_id(&id).await?)
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

async fn require_bearer_user(
    state: &OAuthState,
    headers: &HeaderMap,
) -> Result<microwave_store::User, OAuthError> {
    let token = bearer_token(headers)
        .ok_or_else(|| OAuthError::unauthorized("bearer token is required"))?;
    bearer_user(state, &token)
        .await?
        .ok_or_else(|| OAuthError::unauthorized("invalid bearer token"))
}
