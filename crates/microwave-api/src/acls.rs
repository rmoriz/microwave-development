use crate::helpers::{is_superuser, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, Query, State};
use axum::routing::{get, put};
use axum::{Extension, Json, Router};
use microwave_store::{Org, Permission};
use serde_json::{json, Map, Value};
use std::collections::HashMap;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/users/{username}/_acl", get(get_user_acl))
        .route("/users/{username}/_acl/{permission}", put(put_user_acl))
        .route(
            "/organizations/{organization}/organizations/_acl",
            get(get_organization_acl),
        )
        .route(
            "/organizations/{organization}/organizations/_acl/{permission}",
            put(put_organization_acl),
        )
        // Upstream compatibility route; it always addresses the organization.
        .route(
            "/organizations/{organization}/{scope}/_acl",
            get(get_organization_acl_compat),
        )
        .route(
            "/organizations/{organization}/cookbooks/{name}/_acl",
            get(get_cookbook_acl),
        )
        .route(
            "/organizations/{organization}/cookbooks/{name}/_acl/{permission}",
            put(put_cookbook_acl),
        )
        .route(
            "/organizations/{organization}/cookbook_artifacts/{name}/_acl",
            get(get_cookbook_artifact_acl),
        )
        .route(
            "/organizations/{organization}/cookbook_artifacts/{name}/_acl/{permission}",
            put(put_cookbook_artifact_acl),
        )
        .route(
            "/organizations/{organization}/data/{name}/_acl",
            get(get_data_acl),
        )
        .route(
            "/organizations/{organization}/data/{name}/_acl/{permission}",
            put(put_data_acl),
        )
        .route(
            "/organizations/{organization}/{object_type}/{name}/_acl",
            get(get_object_acl),
        )
        .route(
            "/organizations/{organization}/{object_type}/{name}/_acl/{permission}",
            put(put_object_acl),
        )
}

async fn get_user_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path(username): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let user = state
        .store
        .find_user(&username)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("user {username}")))?;
    require_user_acl_access(&state, &auth, &user.id, &user.authz_id).await?;
    acl_json(&state, &user.authz_id, is_granular(&query)).await
}

async fn put_user_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((username, permission)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let permission = parse_permission(&permission)?;
    let user = state
        .store
        .find_user(&username)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("user {username}")))?;
    require_user_acl_access(&state, &auth, &user.id, &user.authz_id).await?;
    let ace = parse_ace(body, permission)?;
    enforce_grant_constraint(&state, &auth, &user.authz_id, permission, &ace.groups).await?;
    apply_ace(&state, &user.authz_id, permission, &ace).await?;
    Ok(Json(json!({})))
}

async fn get_organization_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    require_grant(&state, &auth, &org.authz_id).await?;
    acl_json(&state, &org.authz_id, is_granular(&query)).await
}

async fn get_organization_acl_compat(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, _scope)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    require_grant(&state, &auth, &org.authz_id).await?;
    acl_json(&state, &org.authz_id, is_granular(&query)).await
}

async fn put_organization_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, permission)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let permission = parse_permission(&permission)?;
    let org = organization(&auth)?;
    require_grant(&state, &auth, &org.authz_id).await?;
    let ace = parse_ace(body, permission)?;
    enforce_grant_constraint(&state, &auth, &org.authz_id, permission, &ace.groups).await?;
    apply_ace(&state, &org.authz_id, permission, &ace).await?;
    Ok(Json(json!({})))
}

async fn get_object_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, object_type, name)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    get_object_acl_inner(&state, &auth, &object_type, &name, &query).await
}

async fn get_cookbook_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    get_object_acl_inner(&state, &auth, "cookbooks", &name, &query).await
}

async fn get_cookbook_artifact_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    get_object_acl_inner(&state, &auth, "cookbook_artifacts", &name, &query).await
}

async fn put_cookbook_artifact_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name, permission)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    put_object_acl_inner(
        &state,
        &auth,
        "cookbook_artifacts",
        &name,
        &permission,
        body,
    )
    .await
}

async fn get_data_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    get_object_acl_inner(&state, &auth, "data", &name, &query).await
}

async fn get_object_acl_inner(
    state: &AppState,
    auth: &AuthContext,
    object_type: &str,
    name: &str,
    query: &HashMap<String, String>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(auth)?;
    let target = resolve_target(state, org, object_type, name).await?;
    require_grant(state, auth, &target).await?;
    acl_json(state, &target, is_granular(query)).await
}

async fn put_object_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, object_type, name, permission)): Path<(String, String, String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    put_object_acl_inner(&state, &auth, &object_type, &name, &permission, body).await
}

async fn put_cookbook_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name, permission)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    put_object_acl_inner(&state, &auth, "cookbooks", &name, &permission, body).await
}

async fn put_data_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name, permission)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    put_object_acl_inner(&state, &auth, "data", &name, &permission, body).await
}

async fn put_object_acl_inner(
    state: &AppState,
    auth: &AuthContext,
    object_type: &str,
    name: &str,
    permission: &str,
    body: Value,
) -> Result<Json<Value>, ApiError> {
    let permission = parse_permission(permission)?;
    let org = organization(auth)?;
    let target = resolve_target(state, org, object_type, name).await?;
    require_grant(state, auth, &target).await?;
    let ace = parse_ace(body, permission)?;
    enforce_grant_constraint(state, auth, &target, permission, &ace.groups).await?;
    apply_ace(state, &target, permission, &ace).await?;
    Ok(Json(json!({})))
}

async fn resolve_target(
    state: &AppState,
    org: &Org,
    object_type: &str,
    name: &str,
) -> Result<String, ApiError> {
    if object_type == "organizations" {
        return Ok(org.authz_id.clone());
    }
    state
        .store
        .find_acl_target(&org.id, object_type, name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("{object_type} {name}")))
}

async fn require_grant(state: &AppState, auth: &AuthContext, target: &str) -> Result<(), ApiError> {
    if is_superuser(auth) {
        return Ok(());
    }
    require_permission(state, auth, target, Permission::Grant).await
}

/// User ACLs are global. Upstream permits the superuser, anyone with a grant
/// ACE on the user, and admins of an organization the user belongs to.
async fn require_user_acl_access(
    state: &AppState,
    auth: &AuthContext,
    user_id: &str,
    user_authz_id: &str,
) -> Result<(), ApiError> {
    if is_superuser(auth) {
        return Ok(());
    }
    if state
        .store
        .check_permission(&auth.principal.authz_id, user_authz_id, Permission::Grant)
        .await
        .map_err(ApiError::from)?
    {
        return Ok(());
    }
    if state
        .store
        .is_org_admin_for_user(&auth.principal.authz_id, user_id)
        .await
        .map_err(ApiError::from)?
    {
        return Ok(());
    }
    Err(ApiError::forbidden())
}

/// Removing the `admins` group from a grant ACE is reserved for the superuser.
async fn enforce_grant_constraint(
    state: &AppState,
    auth: &AuthContext,
    target: &str,
    permission: Permission,
    new_groups: &[String],
) -> Result<(), ApiError> {
    if permission != Permission::Grant || is_superuser(auth) {
        return Ok(());
    }
    let entries = state
        .store
        .get_acl_entries(target)
        .await
        .map_err(ApiError::from)?;
    let current_groups = entries
        .iter()
        .find(|entry| entry.permission == Permission::Grant)
        .map(|entry| entry.groups.as_slice())
        .unwrap_or(&[]);
    if current_groups.iter().any(|group| group == "admins")
        && !new_groups.iter().any(|group| group == "admins")
    {
        return Err(ApiError::forbidden());
    }
    Ok(())
}

async fn apply_ace(
    state: &AppState,
    target: &str,
    permission: Permission,
    ace: &Ace,
) -> Result<(), ApiError> {
    match &ace.legacy_actors {
        Some(actors) => state
            .store
            .set_acl_for_permission(target, permission, actors, &ace.groups)
            .await
            .map_err(acl_store_error),
        None => state
            .store
            .set_acl_for_permission_split(target, permission, &ace.users, &ace.clients, &ace.groups)
            .await
            .map_err(acl_store_error),
    }
}

fn acl_store_error(error: microwave_store::StoreError) -> ApiError {
    if let microwave_store::StoreError::Invalid(message) = &error {
        if message.contains("exists as both a client and a user") {
            return ApiError::unprocessable(message.clone());
        }
    }
    ApiError::from(error)
}

fn parse_permission(value: &str) -> Result<Permission, ApiError> {
    Permission::parse(value)
        .ok_or_else(|| ApiError::bad_request(format!("invalid permission {value}")))
}

fn is_granular(query: &HashMap<String, String>) -> bool {
    query.get("detail").map(String::as_str) == Some("granular")
}

async fn acl_json(
    state: &AppState,
    target_authz_id: &str,
    granular: bool,
) -> Result<Json<Value>, ApiError> {
    let entries = state
        .store
        .get_acl_entries(target_authz_id)
        .await
        .map_err(ApiError::from)?;
    let mut acl = Map::new();
    for entry in entries {
        let mut ace = Map::new();
        if granular {
            ace.insert("actors".into(), json!([]));
            ace.insert("users".into(), json!(entry.users));
            ace.insert("clients".into(), json!(entry.clients));
        } else {
            let mut actors = entry.clients;
            actors.extend(entry.users);
            ace.insert("actors".into(), json!(actors));
        }
        ace.insert("groups".into(), json!(entry.groups));
        acl.insert(entry.permission.as_str().into(), Value::Object(ace));
    }
    Ok(Json(Value::Object(acl)))
}

struct Ace {
    legacy_actors: Option<Vec<String>>,
    users: Vec<String>,
    clients: Vec<String>,
    groups: Vec<String>,
}

fn parse_ace(mut body: Value, permission: Permission) -> Result<Ace, ApiError> {
    if let Some(object) = body.as_object() {
        let has_ace_keys = ["actors", "users", "clients", "groups"]
            .iter()
            .any(|key| object.contains_key(*key));
        if !has_ace_keys {
            if let Some(value) = object.get(permission.as_str()) {
                body = value.clone();
            }
        }
    }
    let object = body
        .as_object()
        .ok_or_else(|| ApiError::bad_request("ACL body must be a JSON object"))?;

    let actors = object
        .get("actors")
        .ok_or_else(|| ApiError::bad_request("ACL body requires actors"))
        .and_then(|value| string_array(value, "actors"))?;
    let groups = object
        .get("groups")
        .ok_or_else(|| ApiError::bad_request("ACL body requires groups"))
        .and_then(|value| string_array(value, "groups"))?;
    let users = object
        .get("users")
        .map(|value| string_array(value, "users"))
        .transpose()?;
    let clients = object
        .get("clients")
        .map(|value| string_array(value, "clients"))
        .transpose()?;

    match (users, clients) {
        (None, None) => Ok(Ace {
            legacy_actors: Some(actors),
            users: Vec::new(),
            clients: Vec::new(),
            groups,
        }),
        (Some(users), Some(clients)) => {
            if !actors.is_empty() {
                return Err(ApiError::bad_request(
                    "actors must be empty when users and clients are supplied",
                ));
            }
            Ok(Ace {
                legacy_actors: None,
                users,
                clients,
                groups,
            })
        }
        _ => Err(ApiError::bad_request(
            "ACL body must contain both users and clients",
        )),
    }
}

fn string_array(value: &Value, field: &str) -> Result<Vec<String>, ApiError> {
    let array = value
        .as_array()
        .ok_or_else(|| ApiError::bad_request(format!("{field} must be an array")))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(ToString::to_string)
                .ok_or_else(|| ApiError::bad_request(format!("{field} must contain strings")))
        })
        .collect()
}
