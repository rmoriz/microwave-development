use crate::helpers::{base_url, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, put};
use axum::{Extension, Json, Router};
use microwave_store::{Invitation, Org, Permission, User};
use serde_json::{json, Map, Value};
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
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    require_permission(&state, &auth, &org.authz_id, Permission::Read)?;
    let base = base_url(&headers, &state);
    let mut users = Map::new();
    for user in state
        .store
        .list_org_users(&org.id)
        .map_err(ApiError::from)?
    {
        users.insert(
            user.username.clone(),
            json!(format!(
                "{base}/organizations/{}/users/{}",
                org.name, user.username
            )),
        );
    }
    Ok(Json(Value::Object(users)))
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

    let user = find_user(&state, &username)?;
    if state
        .store
        .is_user_in_org(&org.id, &user.id)
        .map_err(ApiError::from)?
    {
        return Err(ApiError::conflict("The association already exists."));
    }

    state
        .store
        .add_user_to_org(&org.id, &user.id)
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
    Path((_organization, username)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    require_permission(&state, &auth, &org.authz_id, Permission::Read)?;
    let user = find_user(&state, &username)?;
    ensure_member(&state, org, &user)?;
    Ok(Json(crate::users::user_json(&state, &user)?))
}

async fn remove_org_user(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, username)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let user = find_user(&state, &username)?;
    ensure_member(&state, org, &user)?;

    if auth.principal.authz_id != user.authz_id {
        require_permission(&state, &auth, &org.authz_id, Permission::Update)?;
    }

    state
        .store
        .remove_user_from_org(&org.id, &user.id, &user.username)
        .map_err(ApiError::from)?;
    Ok(Json(json!({})))
}

async fn list_org_invitations(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    require_permission(&state, &auth, &org.authz_id, Permission::Read)?;
    let invitations = state
        .store
        .list_org_invitations(&org.id)
        .map_err(ApiError::from)?;
    Ok(Json(invitations_json(&invitations)))
}

async fn create_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    require_permission(&state, &auth, &org.authz_id, Permission::Update)?;
    let username = username_from_body(&body)?;
    let user = find_user(&state, &username)?;

    if state
        .store
        .is_user_in_org(&org.id, &user.id)
        .map_err(ApiError::from)?
    {
        return Err(ApiError::conflict("The association already exists."));
    }

    let invitation = state
        .store
        .create_invitation(&org.id, &user.id, &auth.principal.authz_id)
        .map_err(ApiError::from)?;
    let base = base_url(&headers, &state);
    let mut response = invitation_json(&invitation);
    response["uri"] = json!(format!(
        "{base}/organizations/{}/association_requests/{}",
        org.name, invitation.id
    ));
    Ok((StatusCode::CREATED, Json(response)))
}

async fn delete_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let invitation = find_invitation(&state, &id)?;
    if invitation.org_id != org.id {
        return Err(invitation_not_found(&id));
    }
    require_permission(&state, &auth, &org.authz_id, Permission::Update)?;
    state.store.delete_invitation(&id).map_err(ApiError::from)?;
    Ok(Json(json!({})))
}

async fn list_user_orgs(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let user = find_user(&state, &username)?;
    let mut orgs = state
        .store
        .list_user_orgs(&user.id)
        .map_err(ApiError::from)?;

    if !is_self_or_superuser(&auth, &user) {
        let requestor_orgs: HashSet<String> = state
            .store
            .list_user_orgs(&auth.principal.actor_id)
            .map_err(ApiError::from)?
            .into_iter()
            .map(|org| org.id)
            .collect();
        orgs.retain(|org| requestor_orgs.contains(&org.id));
        if orgs.is_empty() {
            return Err(ApiError::forbidden());
        }
    }

    let base = base_url(&headers, &state);
    let mut result = Map::new();
    for org in orgs {
        result.insert(
            org.name.clone(),
            json!(format!("{base}/organizations/{}", org.name)),
        );
    }
    Ok(Json(Value::Object(result)))
}

async fn list_user_invitations(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(username): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = find_user(&state, &username)?;
    require_self_or_superuser(&auth, &user)?;
    let invitations = state
        .store
        .list_user_invitations(&user.id)
        .map_err(ApiError::from)?;
    Ok(Json(invitations_json(&invitations)))
}

async fn count_user_invitations(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(username): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = find_user(&state, &username)?;
    require_self_or_superuser(&auth, &user)?;
    let count = state
        .store
        .count_user_invitations(&user.id)
        .map_err(ApiError::from)?;
    Ok(Json(json!({ "value": count })))
}

async fn respond_to_invitation(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((username, id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let user = find_user(&state, &username)?;
    let invitation = find_invitation(&state, &id)?;
    if invitation.user_id != user.id {
        return Err(invitation_not_found(&id));
    }

    if !is_self_or_superuser(&auth, &user) {
        require_permission(&state, &auth, &user.authz_id, Permission::Grant)?;
    }

    let response = body
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let invitation = match response {
        "accept" => state.store.accept_invitation(&id).map_err(ApiError::from)?,
        "reject" => state.store.reject_invitation(&id).map_err(ApiError::from)?,
        _ => {
            return Err(ApiError::bad_request(
                "Param response must be either 'accept' or 'reject'",
            ))
        }
    };
    Ok(Json(invitation_json(&invitation)))
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

fn find_user(state: &AppState, username: &str) -> Result<User, ApiError> {
    state
        .store
        .find_user(username)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("user {username}")))
}

fn find_invitation(state: &AppState, id: &str) -> Result<Invitation, ApiError> {
    state
        .store
        .find_invitation(id)
        .map_err(ApiError::from)?
        .ok_or_else(|| invitation_not_found(id))
}

fn invitation_not_found(id: &str) -> ApiError {
    ApiError::not_found(format!("Cannot find association request: {id}"))
}

fn ensure_member(state: &AppState, org: &Org, user: &User) -> Result<(), ApiError> {
    if state
        .store
        .is_user_in_org(&org.id, &user.id)
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

fn invitation_json(invitation: &Invitation) -> Value {
    json!({
        "id": invitation.id,
        "org": invitation.org_name,
        "user": invitation.username
    })
}

fn invitations_json(invitations: &[Invitation]) -> Value {
    Value::Array(invitations.iter().map(invitation_json).collect())
}

fn is_superuser(auth: &AuthContext) -> bool {
    auth.principal.name == "pivotal"
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
