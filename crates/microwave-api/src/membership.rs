use crate::helpers::{base_url, organization, require_permission};
use crate::{ApiError, ApiVersion, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, put};
use axum::{Extension, Json, Router};
use microwave_store::{Invitation, Org, Permission, User};
use serde_json::{json, Value};
use std::collections::HashSet;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/users",
            get(list_org_users).post(add_org_user),
        )
        .route(
            "/organizations/{organization}/users/{user}",
            get(get_org_user).delete(remove_org_user),
        )
        .route(
            "/organizations/{organization}/association_requests",
            get(list_org_invitations).post(create_invitation),
        )
        .route(
            "/organizations/{organization}/association_requests/{id}",
            delete(delete_invitation),
        )
        .route("/users/{user}/organizations", get(list_user_orgs))
        .route(
            "/users/{user}/association_requests",
            get(list_user_invitations),
        )
        .route(
            "/users/{user}/association_requests/count",
            get(count_user_invitations),
        )
        .route(
            "/users/{user}/association_requests/{id}",
            put(respond_to_invitation),
        )
}

async fn list_org_users(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    require_permission(&state, &auth, &org.authz_id, Permission::Read).await?;
    let users = state
        .store
        .list_org_users(&org.id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(Value::Array(
        users
            .iter()
            .map(|user| json!({ "user": { "username": user.username } }))
            .collect(),
    )))
}

async fn add_org_user(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    require_superuser(&auth)?;
    let username = username_from_body(&body)?;

    let user = find_user(&state, &username).await?;
    if state
        .store
        .is_user_in_org(&org.id, &user.id)
        .await
        .map_err(ApiError::from)?
    {
        // Upstream emits this conflict as a plain string; pedant's platform
        // helper compares the value directly when accepting a repeat
        // association.
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({ "error": "The association already exists." })),
        ));
    }

    state
        .store
        .add_user_to_org(&org.id, &user.id)
        .await
        .map_err(ApiError::from)?;

    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}/users/{}", org.name, user.username)
        })),
    ))
}

async fn get_org_user(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    Path((_organization, username)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    require_permission(&state, &auth, &org.authz_id, Permission::Read).await?;
    let user = find_user(&state, &username).await?;
    ensure_member(&state, org, &user).await?;
    Ok(Json(
        crate::users::user_json(&state, &user, api_version).await?,
    ))
}

async fn remove_org_user(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Extension(ApiVersion(api_version)): Extension<ApiVersion>,
    Path((_organization, username)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let user = find_user(&state, &username).await?;
    ensure_member(&state, org, &user).await?;

    if auth.principal.authz_id != user.authz_id {
        require_permission(&state, &auth, &org.authz_id, Permission::Update).await?;
    }

    state
        .store
        .remove_user_from_org(&org.id, &user.id, &user.username)
        .await
        .map_err(ApiError::from)?;
    // Upstream returns the removed user's document so the caller can see what
    // was removed instead of an empty object.
    Ok(Json(
        crate::users::user_json(&state, &user, api_version).await?,
    ))
}

async fn list_org_invitations(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    require_permission(&state, &auth, &org.authz_id, Permission::Read).await?;
    let invitations = state
        .store
        .list_org_invitations(&org.id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(org_invitations_json(&invitations)))
}

async fn create_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    require_permission(&state, &auth, &org.authz_id, Permission::Update).await?;
    let username = username_from_body(&body)?;
    let user = find_user(&state, &username).await?;

    if state
        .store
        .is_user_in_org(&org.id, &user.id)
        .await
        .map_err(ApiError::from)?
    {
        return Err(ApiError::conflict("The association already exists."));
    }

    let invitation = state
        .store
        .create_invitation(&org.id, &user.id, &auth.principal.authz_id)
        .await
        .map_err(ApiError::from)?;
    let base = base_url(&headers, &state);
    // Upstream's create response nests the inviting user, the organization and
    // the invited user alongside the URI of the new invitation.
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!(
                "{base}/organizations/{}/association_requests/{}",
                org.name, invitation.id
            ),
            "organization_user": { "username": auth.principal.name },
            "organization": { "name": org.name },
            "user": {
                "email": user.email.clone().unwrap_or_default(),
                "first_name": user.first_name.clone().unwrap_or_default(),
            },
        })),
    ))
}

async fn delete_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let invitation = find_invitation(&state, &id).await?;
    if invitation.org_id != org.id {
        return Err(invitation_not_found(&id));
    }
    require_permission(&state, &auth, &org.authz_id, Permission::Update).await?;
    state
        .store
        .delete_invitation(&id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(invitation_response_json(&invitation)))
}

async fn list_user_orgs(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(username): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = find_user(&state, &username).await?;
    let mut orgs = state
        .store
        .list_user_orgs(&user.id)
        .await
        .map_err(ApiError::from)?;

    if !is_self_or_superuser(&auth, &user) {
        let requestor_orgs: HashSet<String> = state
            .store
            .list_user_orgs(&auth.principal.actor_id)
            .await
            .map_err(ApiError::from)?
            .into_iter()
            .map(|org| org.id)
            .collect();
        orgs.retain(|org| requestor_orgs.contains(&org.id));
        if orgs.is_empty() {
            return Err(ApiError::forbidden());
        }
    }

    // Upstream returns a list of `{"organization": {...}}` wrappers. The guid
    // is the same stable organization id used by the organization endpoints.
    Ok(Json(Value::Array(
        orgs.iter()
            .map(|org| {
                json!({
                    "organization": {
                        "name": org.name,
                        "full_name": org.full_name,
                        "guid": org.id,
                    }
                })
            })
            .collect(),
    )))
}

async fn list_user_invitations(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(username): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = find_user(&state, &username).await?;
    require_self_or_superuser(&auth, &user)?;
    let invitations = state
        .store
        .list_user_invitations(&user.id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(user_invitations_json(&invitations)))
}

async fn count_user_invitations(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(username): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = find_user(&state, &username).await?;
    require_self_or_superuser(&auth, &user)?;
    let count = state
        .store
        .count_user_invitations(&user.id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(json!({ "value": count })))
}

async fn respond_to_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((username, id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let user = find_user(&state, &username).await?;
    let invitation = find_invitation(&state, &id).await?;
    if invitation.user_id != user.id {
        return Err(invitation_not_found(&id));
    }

    if !is_self_or_superuser(&auth, &user) {
        require_permission(&state, &auth, &user.authz_id, Permission::Grant).await?;
    }

    let response = body
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let invitation = match response {
        "accept" => state
            .store
            .accept_invitation(&id)
            .await
            .map_err(ApiError::from)?,
        "reject" => state
            .store
            .reject_invitation(&id)
            .await
            .map_err(ApiError::from)?,
        _ => {
            return Err(ApiError::bad_request(
                "Param response must be either 'accept' or 'reject'",
            ))
        }
    };
    Ok(Json(invitation_response_json(&invitation)))
}

fn username_from_body(body: &Value) -> Result<String, ApiError> {
    let username = body
        .get("username")
        .and_then(Value::as_str)
        .or_else(|| body.get("user").and_then(user_name))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("username is required"))?;
    Ok(username.to_string())
}

fn user_name(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("username").and_then(Value::as_str))
}

async fn find_user(state: &AppState, username: &str) -> Result<User, ApiError> {
    state
        .store
        .find_user(username)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("user {username}")))
}

async fn find_invitation(state: &AppState, id: &str) -> Result<Invitation, ApiError> {
    state
        .store
        .find_invitation(id)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| invitation_not_found(id))
}

fn invitation_not_found(id: &str) -> ApiError {
    ApiError::not_found(format!("Cannot find association request: {id}"))
}

async fn ensure_member(state: &AppState, org: &Org, user: &User) -> Result<(), ApiError> {
    if state
        .store
        .is_user_in_org(&org.id, &user.id)
        .await
        .map_err(ApiError::from)?
    {
        Ok(())
    } else {
        Err(ApiError::not_found(format!(
            "Cannot find a user {} in organization {}",
            user.username, org.name
        )))
    }
}

/// Organization-scoped invitation listing: `[{"id": ..., "username": ...}]`.
fn org_invitations_json(invitations: &[Invitation]) -> Value {
    Value::Array(
        invitations
            .iter()
            .map(|invitation| {
                json!({
                    "id": invitation.id,
                    "username": invitation.username,
                })
            })
            .collect(),
    )
}

/// User-scoped invitation listing: `[{"id": ..., "orgname": ...}]`.
fn user_invitations_json(invitations: &[Invitation]) -> Value {
    Value::Array(
        invitations
            .iter()
            .map(|invitation| {
                json!({
                    "id": invitation.id,
                    "orgname": invitation.org_name,
                })
            })
            .collect(),
    )
}

/// The upstream invitation document returned when an invitation is accepted,
/// rejected, or deleted. The `organization`, `user`, and
/// `organization_admin_actor_id` fields are retained as deprecated markers.
fn invitation_response_json(invitation: &Invitation) -> Value {
    json!({
        "id": invitation.id,
        "orgname": invitation.org_name,
        "username": invitation.username,
        "organization": {
            "deprecated": true,
            "name": invitation.org_name,
        },
        "user": "deprecated",
        "organization_admin_actor_id": "deprecated",
    })
}

fn is_superuser(auth: &AuthContext) -> bool {
    auth.is_superuser
}

fn is_self_or_superuser(auth: &AuthContext, user: &User) -> bool {
    auth.principal.authz_id == user.authz_id || is_superuser(auth)
}

fn require_superuser(auth: &AuthContext) -> Result<(), ApiError> {
    if is_superuser(auth) {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}

fn require_self_or_superuser(auth: &AuthContext, user: &User) -> Result<(), ApiError> {
    if is_self_or_superuser(auth, user) {
        Ok(())
    } else {
        Err(ApiError::forbidden())
    }
}
