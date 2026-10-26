use crate::chef_json::ChefJson;
use crate::helpers::{
    base_url, container_authz, location_headers, organization, require_permission,
};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::Permission;
use serde_json::{json, Map, Value};

/// Top-level keys accepted on role payloads, mirroring `?VALID_KEYS` in
/// `chef_role.erl`.
const VALID_KEYS: [&str; 8] = [
    "chef_type",
    "default_attributes",
    "description",
    "env_run_lists",
    "json_class",
    "name",
    "override_attributes",
    "run_list",
];

const ATTRIBUTE_KEYS: [&str; 2] = ["default_attributes", "override_attributes"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/roles",
            get(list_roles).post(create_role),
        )
        .route(
            "/organizations/{organization}/roles/{role}",
            get(get_role).put(update_role).delete(delete_role),
        )
        .route(
            "/organizations/{organization}/roles/{role}/environments",
            get(list_role_environments),
        )
        .route(
            "/organizations/{organization}/roles/{role}/environments/{environment}",
            get(get_role_environment),
        )
        .route(
            "/organizations/{organization}/environments/{environment}/roles/{role}",
            get(get_environment_role),
        )
}

async fn list_roles(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "roles").await?;
    require_permission(&state, &auth, &container, Permission::Read).await?;

    let base = base_url(&headers, &state);
    let mut roles = serde_json::Map::new();
    for role in state
        .store
        .list_roles(&org.id)
        .await
        .map_err(ApiError::from)?
    {
        roles.insert(
            role.name.clone(),
            json!(format!(
                "{base}/organizations/{}/roles/{}",
                org.name, role.name
            )),
        );
    }
    Ok(Json(Value::Object(roles)))
}

async fn create_role(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    ChefJson(mut body): ChefJson<Value>,
) -> Result<(StatusCode, HeaderMap, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "roles").await?;
    require_permission(&state, &auth, &container, Permission::Create).await?;

    let name = normalize_role(&mut body, None)?;
    let role = state
        .store
        .create_role(
            &org.id,
            &name,
            &serde_json::to_string(&body).map_err(|_| ApiError::internal())?,
            &auth.principal.authz_id,
        )
        .await
        .map_err(ApiError::from)?;
    crate::search::index::index_object(&state, org, crate::search::IndexKind::Role, &role).await;

    let base = base_url(&headers, &state);
    let uri = format!("{base}/organizations/{}/roles/{name}", org.name);
    Ok((
        StatusCode::CREATED,
        location_headers(&uri)?,
        Json(json!({ "uri": uri })),
    ))
}

async fn get_role(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let role = state
        .store
        .find_role(&org.id, &name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("role {name}")))?;
    require_permission(&state, &auth, &role.authz_id, Permission::Read).await?;
    Ok(Json(role_document(&role)?))
}

/// `GET /organizations/:org/roles/:role/environments`
///
/// Upstream always includes `_default`, then every environment named by the
/// role's `env_run_lists`, sorted and de-duplicated. The referenced
/// environments do not need to exist.
async fn list_role_environments(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let role = find_role(&state, org, &name).await?;
    require_permission(&state, &auth, &role.authz_id, Permission::Read).await?;

    let document = role_document(&role)?;
    let mut environments = vec!["_default".to_string()];
    if let Some(run_lists) = document.get("env_run_lists").and_then(Value::as_object) {
        environments.extend(run_lists.keys().cloned());
    }
    environments.sort();
    environments.dedup();

    Ok(Json(json!(environments)))
}

/// `GET /organizations/:org/roles/:role/environments/:environment`
async fn get_role_environment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name, environment)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    role_environment_run_list(&state, &auth, org, &name, &environment).await
}

/// `GET /organizations/:org/environments/:environment/roles/:role`
async fn get_environment_role(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, environment, name)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    role_environment_run_list(&state, &auth, org, &name, &environment).await
}

/// Shared implementation for both environment-role routes.
///
/// Order matters: upstream reports a missing role before a missing
/// environment, and only checks read permission after both have been found.
async fn role_environment_run_list(
    state: &AppState,
    auth: &AuthContext,
    org: &microwave_store::Org,
    role_name: &str,
    environment_name: &str,
) -> Result<Json<Value>, ApiError> {
    let role = find_role(state, org, role_name).await?;
    if environment_name != "_default"
        && state
            .store
            .find_environment(&org.id, environment_name)
            .await
            .map_err(ApiError::from)?
            .is_none()
    {
        return Err(ApiError::not_found(format!(
            "Cannot load environment {environment_name}"
        )));
    }
    require_permission(state, auth, &role.authz_id, Permission::Read).await?;

    let document = role_document(&role)?;
    let run_list = if environment_name == "_default" {
        document.get("run_list").cloned().unwrap_or(Value::Null)
    } else {
        document
            .get("env_run_lists")
            .and_then(|lists| lists.get(environment_name))
            .cloned()
            .unwrap_or(Value::Null)
    };

    Ok(Json(json!({ "run_list": run_list })))
}

async fn find_role(
    state: &AppState,
    org: &microwave_store::Org,
    name: &str,
) -> Result<microwave_store::StoredObject, ApiError> {
    state
        .store
        .find_role(&org.id, name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("Cannot load role {name}")))
}

fn role_document(role: &microwave_store::StoredObject) -> Result<Value, ApiError> {
    let mut document: Value =
        serde_json::from_str(&role.serialized_object).map_err(|_| ApiError::internal())?;
    normalize_role_document(&mut document);
    Ok(document)
}

async fn update_role(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    ChefJson(mut body): ChefJson<Value>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let role = state
        .store
        .find_role(&org.id, &name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("role {name}")))?;
    require_permission(&state, &auth, &role.authz_id, Permission::Update).await?;

    normalize_role(&mut body, Some(&name))?;
    let updated = state
        .store
        .update_role(
            &org.id,
            &name,
            &serde_json::to_string(&body).map_err(|_| ApiError::internal())?,
        )
        .await
        .map_err(ApiError::from)?;
    crate::search::index::index_object(&state, org, crate::search::IndexKind::Role, &updated).await;
    Ok(Json(role_document(&updated)?))
}

async fn delete_role(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let org = organization(&auth)?;
    let role = state
        .store
        .find_role(&org.id, &name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("role {name}")))?;
    require_permission(&state, &auth, &role.authz_id, Permission::Delete).await?;
    let value = role_document(&role)?;
    state
        .store
        .delete_role(&org.id, &name)
        .await
        .map_err(ApiError::from)?;
    crate::search::index::delete_document(&state, &org.id, &role.id).await;
    Ok(Json(value).into_response())
}

/// Validates and normalizes a role payload following
/// `chef_role:parse_binary_json/2`.
///
/// `path_name` is `Some` for PUT requests: the body name is optional, but when
/// present it must match the URL. Create requests require an explicit name.
fn normalize_role(body: &mut Value, path_name: Option<&str>) -> Result<String, ApiError> {
    let Some(object) = body.as_object_mut() else {
        return Err(ApiError::bad_request(
            "Incorrect JSON type for request body",
        ));
    };

    let name = match path_name {
        Some(path_name) => {
            match object.get("name") {
                None => {
                    object.insert("name".into(), json!(path_name));
                }
                Some(Value::String(body_name)) if body_name == path_name => {}
                Some(_) => {
                    return Err(ApiError::bad_request("Role name mismatch."));
                }
            }
            path_name.to_string()
        }
        None => object
            .get("name")
            .ok_or_else(|| ApiError::bad_request("Field 'name' missing"))?
            .as_str()
            .ok_or_else(|| ApiError::bad_request("Field 'name' invalid"))?
            .to_string(),
    };
    if !valid_name(&name) {
        return Err(ApiError::bad_request("Field 'name' invalid"));
    }
    object.insert("name".into(), json!(name));

    if let Some(key) = object
        .keys()
        .find(|key| !VALID_KEYS.contains(&key.as_str()))
    {
        return Err(ApiError::bad_request(format!(
            "Invalid key {key} in request body"
        )));
    }

    validate_fixed_field(object, "json_class", "Chef::Role")?;
    validate_fixed_field(object, "chef_type", "role")?;

    for key in ATTRIBUTE_KEYS {
        match object.get(key) {
            None => {
                object.insert(key.into(), json!({}));
            }
            Some(Value::Object(_)) => {}
            Some(_) => {
                return Err(ApiError::bad_request(format!(
                    "Field '{key}' is not a hash"
                )));
            }
        }
    }

    let run_list = match object.get("run_list") {
        None => Vec::new(),
        Some(value) => validate_run_list(value, "Field 'run_list' is not a valid run list")?,
    };
    object.insert("run_list".into(), json!(normalize_run_list(run_list)));

    let env_run_lists = match object.get("env_run_lists") {
        None => Map::new(),
        Some(Value::Object(lists)) => {
            let mut normalized = Map::new();
            for (environment, run_list) in lists {
                if !valid_name(environment) {
                    return Err(ApiError::bad_request(format!(
                        "Invalid key '{environment}' for env_run_lists"
                    )));
                }
                let run_list = validate_run_list(
                    run_list,
                    "Field 'env_run_lists' contains invalid run lists",
                )?;
                normalized.insert(environment.clone(), json!(normalize_run_list(run_list)));
            }
            normalized
        }
        Some(_) => {
            return Err(ApiError::bad_request(
                "Field 'env_run_lists' contains invalid run lists",
            ));
        }
    };
    object.insert("env_run_lists".into(), Value::Object(env_run_lists));

    Ok(name)
}

fn validate_fixed_field(
    object: &mut Map<String, Value>,
    key: &str,
    expected: &str,
) -> Result<(), ApiError> {
    match object.get(key) {
        None => {
            object.insert(key.into(), json!(expected));
            Ok(())
        }
        Some(Value::String(value)) if value == expected => Ok(()),
        Some(_) => Err(ApiError::bad_request(format!("Field '{key}' invalid"))),
    }
}

fn validate_run_list(value: &Value, message: &str) -> Result<Vec<String>, ApiError> {
    let Some(items) = value.as_array() else {
        return Err(ApiError::bad_request(message));
    };
    let mut run_list = Vec::with_capacity(items.len());
    for item in items {
        let item = item
            .as_str()
            .ok_or_else(|| ApiError::bad_request(message))?;
        if !valid_run_list_item(item) {
            return Err(ApiError::bad_request(message));
        }
        run_list.push(item.to_string());
    }
    Ok(run_list)
}

/// Upstream normalizes every run list item to an explicit `recipe[...]` or
/// `role[...]` and removes exact duplicates after qualification.
fn normalize_run_list(run_list: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::with_capacity(run_list.len());
    for item in run_list {
        let item = if item.starts_with("recipe[") || item.starts_with("role[") {
            item
        } else {
            format!("recipe[{item}]")
        };
        if !normalized.contains(&item) {
            normalized.push(item);
        }
    }
    normalized
}

/// Normalize run lists in a stored document. Writes are already normalized,
/// but reads also cover objects stored before this contract was enforced.
fn normalize_role_document(document: &mut Value) {
    let Some(object) = document.as_object_mut() else {
        return;
    };
    if let Some(run_list) = object.get_mut("run_list") {
        normalize_run_list_value(run_list);
    }
    if let Some(Value::Object(env_run_lists)) = object.get_mut("env_run_lists") {
        for run_list in env_run_lists.values_mut() {
            normalize_run_list_value(run_list);
        }
    }
}

fn normalize_run_list_value(value: &mut Value) {
    let Some(items) = value.as_array() else {
        return;
    };
    if !items.iter().all(Value::is_string) {
        return;
    }
    let run_list = items
        .iter()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect();
    *value = json!(normalize_run_list(run_list));
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
}

fn valid_run_list_item(item: &str) -> bool {
    if let Some(name) = item
        .strip_prefix("recipe[")
        .and_then(|item| item.strip_suffix(']'))
    {
        return valid_recipe(name);
    }
    if let Some(name) = item
        .strip_prefix("role[")
        .and_then(|item| item.strip_suffix(']'))
    {
        return valid_short_name(name);
    }
    valid_recipe(item)
}

/// A recipe reference is `[cookbook::]name[@MAJOR.MINOR[.PATCH]]`.
fn valid_recipe(item: &str) -> bool {
    match item.split_once('@') {
        Some((name, version)) => valid_recipe_name(name) && valid_version(version),
        None => valid_recipe_name(item),
    }
}

fn valid_recipe_name(name: &str) -> bool {
    let mut parts = name.split("::");
    match (parts.next(), parts.next(), parts.next()) {
        (Some(cookbook), Some(recipe), None) => {
            valid_short_name(cookbook) && valid_short_name(recipe)
        }
        (Some(recipe), None, None) => valid_short_name(recipe),
        _ => false,
    }
}

fn valid_short_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    (2..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}
