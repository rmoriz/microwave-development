use crate::helpers::{organization, require_permission};
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
        // Cookbooks and cookbook artifacts also have a versioned child route
        // (`.../<name>/<version>`), so `_acl` must be registered explicitly;
        // otherwise the version handler captures it.
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
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("user {username}")))?;
    require_grant(&state, &auth, &user.authz_id)?;
    acl_json(&state, &user.authz_id, is_granular(&query))
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
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("user {username}")))?;
    require_grant(&state, &auth, &user.authz_id)?;
    let ace = parse_ace(body, permission)?;
    state
        .store
        .set_acl_for_permission(&user.authz_id, permission, &ace.actors, &ace.groups)
        .map_err(ApiError::from)?;
    Ok(Json(json!({})))
}

async fn get_organization_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    require_grant(&state, &auth, &org.authz_id)?;
    acl_json(&state, &org.authz_id, is_granular(&query))
}

async fn get_organization_acl_compat(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, _scope)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    require_grant(&state, &auth, &org.authz_id)?;
    acl_json(&state, &org.authz_id, is_granular(&query))
}

async fn put_organization_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, permission)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let permission = parse_permission(&permission)?;
    let org = organization(&auth)?;
    require_grant(&state, &auth, &org.authz_id)?;
    let ace = parse_ace(body, permission)?;
    state
        .store
        .set_acl_for_permission(&org.authz_id, permission, &ace.actors, &ace.groups)
        .map_err(ApiError::from)?;
    Ok(Json(json!({})))
}

async fn get_object_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, object_type, name)): Path<(String, String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    get_object_acl_for(&state, &auth, &object_type, &name, is_granular(&query))
}

async fn put_object_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, object_type, name, permission)): Path<(String, String, String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    put_object_acl_for(&state, &auth, &object_type, &name, &permission, body)
}

async fn get_cookbook_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    get_object_acl_for(&state, &auth, "cookbooks", &name, is_granular(&query))
}

async fn put_cookbook_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name, permission)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    put_object_acl_for(&state, &auth, "cookbooks", &name, &permission, body)
}

async fn get_cookbook_artifact_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, ApiError> {
    get_object_acl_for(
        &state,
        &auth,
        "cookbook_artifacts",
        &name,
        is_granular(&query),
    )
}

async fn put_cookbook_artifact_acl(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name, permission)): Path<(String, String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    put_object_acl_for(
        &state,
        &auth,
        "cookbook_artifacts",
        &name,
        &permission,
        body,
    )
}

fn get_object_acl_for(
    state: &AppState,
    auth: &AuthContext,
    object_type: &str,
    name: &str,
    granular: bool,
) -> Result<Json<Value>, ApiError> {
    let org = organization(auth)?;
    let target = resolve_target(state, org, object_type, name)?;
    require_grant(state, auth, &target)?;
    acl_json(state, &target, granular)
}

fn put_object_acl_for(
    state: &AppState,
    auth: &AuthContext,
    object_type: &str,
    name: &str,
    permission: &str,
    body: Value,
) -> Result<Json<Value>, ApiError> {
    let permission = parse_permission(permission)?;
    let org = organization(auth)?;
    let target = resolve_target(state, org, object_type, name)?;
    require_grant(state, auth, &target)?;
    let ace = parse_ace(body, permission)?;
    state
        .store
        .set_acl_for_permission(&target, permission, &ace.actors, &ace.groups)
        .map_err(ApiError::from)?;
    Ok(Json(json!({})))
}

fn resolve_target(
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
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("{object_type} {name}")))
}

fn require_grant(state: &AppState, auth: &AuthContext, target: &str) -> Result<(), ApiError> {
    if auth.principal.name == "pivotal" {
        return Ok(());
    }
    require_permission(state, auth, target, Permission::Grant)
}

fn parse_permission(value: &str) -> Result<Permission, ApiError> {
    Permission::parse(value)
        .ok_or_else(|| ApiError::bad_request(format!("invalid permission {value}")))
}

fn is_granular(query: &HashMap<String, String>) -> bool {
    query.get("detail").map(String::as_str) == Some("granular")
}

fn acl_json(
    state: &AppState,
    target_authz_id: &str,
    granular: bool,
) -> Result<Json<Value>, ApiError> {
    let entries = state
        .store
        .get_acl_entries(target_authz_id)
        .map_err(ApiError::from)?;
    let mut acl = Map::new();
    for entry in entries {
        let mut ace = Map::new();
        if granular {
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
    actors: Vec<String>,
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

    let has_actors = object.contains_key("actors");
    let has_users = object.contains_key("users");
    let has_clients = object.contains_key("clients");
    if has_actors && (has_users || has_clients) {
        return Err(ApiError::bad_request(
            "ACL body must not contain both actors and users/clients",
        ));
    }

    let mut actors = Vec::new();
    if has_users || has_clients {
        if let Some(users) = object.get("users") {
            actors.extend(string_array(users, "users")?);
        }
        if let Some(clients) = object.get("clients") {
            actors.extend(string_array(clients, "clients")?);
        }
    } else if let Some(value) = object.get("actors") {
        actors.extend(string_array(value, "actors")?);
    } else {
        return Err(ApiError::bad_request(
            "ACL body requires actors or users/clients",
        ));
    }

    let groups = match object.get("groups") {
        Some(value) => string_array(value, "groups")?,
        None => Vec::new(),
    };

    Ok(Ace { actors, groups })
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
