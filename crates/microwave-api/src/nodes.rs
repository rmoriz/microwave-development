use crate::chef_json::ChefJson;
use crate::helpers::{
    base_url, container_authz, location_headers, organization, require_permission,
};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::{NewNode, Org, Permission, StoredNode};
use serde_json::{json, Map, Value};

/// Top-level keys accepted on node payloads, mirroring `?VALID_KEYS` in
/// `chef_node.erl`. Anything else is rejected with the upstream message.
const VALID_KEYS: [&str; 11] = [
    "name",
    "chef_environment",
    "json_class",
    "chef_type",
    "normal",
    "default",
    "override",
    "automatic",
    "run_list",
    "policy_name",
    "policy_group",
];

const ATTRIBUTE_KEYS: [&str; 4] = ["normal", "default", "override", "automatic"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/nodes",
            get(list_nodes).post(create_node),
        )
        .route(
            "/organizations/{organization}/nodes/",
            get(list_nodes).post(create_node),
        )
        .route(
            "/organizations/{organization}/nodes/{node}",
            get(get_node).put(update_node).delete(delete_node),
        )
        .route(
            "/organizations/{organization}/environments/{environment}/nodes",
            get(list_environment_nodes),
        )
}

async fn list_nodes(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let nodes_container = container_authz(&state, org, "nodes").await?;
    require_permission(&state, &auth, &nodes_container, Permission::Read).await?;

    let nodes = state
        .store
        .list_node_names(&org.id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(node_map(
        &headers,
        &state,
        org,
        nodes.iter().map(String::as_str),
    )))
}

async fn list_environment_nodes(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, environment)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let nodes_container = container_authz(&state, org, "nodes").await?;
    require_permission(&state, &auth, &nodes_container, Permission::Read).await?;

    let exists = state
        .store
        .find_environment(&org.id, &environment)
        .await
        .map_err(ApiError::from)?
        .is_some();
    if !exists {
        return Err(ApiError::not_found(format!(
            "Cannot load environment '{environment}'"
        )));
    }

    let nodes = state
        .store
        .list_nodes_in_environment(&org.id, &environment)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(node_map(
        &headers,
        &state,
        org,
        nodes.iter().map(|n| n.name.as_str()),
    )))
}

fn node_map<'a>(
    headers: &HeaderMap,
    state: &AppState,
    org: &microwave_store::Org,
    names: impl Iterator<Item = &'a str>,
) -> Value {
    let base = base_url(headers, state);
    let mut nodes = Map::new();
    for name in names {
        nodes.insert(
            name.to_string(),
            json!(format!("{base}/organizations/{}/nodes/{}", org.name, name)),
        );
    }
    Value::Object(nodes)
}

async fn create_node(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    ChefJson(mut body): ChefJson<Value>,
) -> Result<(StatusCode, HeaderMap, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let node = create_node_object(&state, &auth, org, &mut body).await?;

    let base = base_url(&headers, &state);
    let uri = format!("{base}/organizations/{}/nodes/{}", org.name, node.name);
    Ok((
        StatusCode::CREATED,
        location_headers(&uri)?,
        Json(json!({ "uri": uri })),
    ))
}

/// Create a node through the shared mutation path used by HTTP and MCP.
pub(crate) async fn create_node_object(
    state: &AppState,
    auth: &AuthContext,
    org: &Org,
    body: &mut Value,
) -> Result<StoredNode, ApiError> {
    let nodes_container = container_authz(state, org, "nodes").await?;
    require_permission(state, auth, &nodes_container, Permission::Create).await?;

    let normalized = normalize_node(body, None)?;
    let node = state
        .store
        .create_node(
            &org.id,
            NewNode {
                name: &normalized.name,
                body: &normalized.serialized,
                environment: &normalized.environment,
                policy_name: normalized.policy_name.as_deref(),
                policy_group: normalized.policy_group.as_deref(),
                creator_authz_id: &auth.principal.authz_id,
            },
        )
        .await
        .map_err(ApiError::from)?;
    crate::search::index::index_node(state, org, &node).await;
    Ok(node)
}

async fn get_node(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let node = state
        .store
        .find_node(&org.id, &name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| node_not_found(&name))?;
    require_permission(&state, &auth, &node.authz_id, Permission::Read).await?;
    serialized_body(&node.serialized_object)
}

async fn update_node(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    ChefJson(mut body): ChefJson<Value>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let node = update_node_object(&state, &auth, org, &name, &mut body).await?;
    serialized_body(&node.serialized_object)
}

/// Update a node through the shared mutation path used by HTTP and MCP.
pub(crate) async fn update_node_object(
    state: &AppState,
    auth: &AuthContext,
    org: &Org,
    name: &str,
    body: &mut Value,
) -> Result<StoredNode, ApiError> {
    let normalized = normalize_node(body, Some(name))?;
    let existing = state
        .store
        .find_node(&org.id, name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| node_not_found(name))?;
    require_permission(state, auth, &existing.authz_id, Permission::Update).await?;

    let node = state
        .store
        .update_node(
            &org.id,
            name,
            &normalized.serialized,
            &normalized.environment,
            normalized.policy_name.as_deref(),
            normalized.policy_group.as_deref(),
        )
        .await
        .map_err(ApiError::from)?;
    crate::search::index::index_node(state, org, &node).await;
    Ok(node)
}

async fn delete_node(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let node = delete_node_object(&state, &auth, org, &name).await?;
    serialized_body(&node.serialized_object)
}

/// Delete a node through the shared mutation path used by HTTP and MCP.
pub(crate) async fn delete_node_object(
    state: &AppState,
    auth: &AuthContext,
    org: &Org,
    name: &str,
) -> Result<StoredNode, ApiError> {
    let node = state
        .store
        .find_node(&org.id, name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| node_not_found(name))?;
    require_permission(state, auth, &node.authz_id, Permission::Delete).await?;
    state
        .store
        .delete_node(&org.id, name)
        .await
        .map_err(ApiError::from)?;
    crate::search::index::delete_document(state, &org.id, &node.id).await;
    Ok(node)
}

fn node_not_found(name: &str) -> ApiError {
    ApiError::not_found(format!("node '{name}' not found"))
}

fn serialized_body(serialized: &str) -> Result<Json<Value>, ApiError> {
    let value = serde_json::from_str(serialized).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
}

pub(crate) struct NormalizedNode {
    pub(crate) name: String,
    pub(crate) environment: String,
    pub(crate) policy_name: Option<String>,
    pub(crate) policy_group: Option<String>,
    pub(crate) serialized: String,
}

/// Validates and normalizes a node payload following the upstream
/// `chef_node:parse_check_binary_as_json_node/2` behaviour.
///
/// `path_name` is `Some` for PUT requests: the body name is optional, but when
/// present it must match the URL. Create requests require an explicit name.
pub(crate) fn normalize_node(
    body: &mut Value,
    path_name: Option<&str>,
) -> Result<NormalizedNode, ApiError> {
    let Some(object) = body.as_object_mut() else {
        return Err(ApiError::bad_request(
            "Incorrect JSON type for request body",
        ));
    };

    if let Some(key) = object
        .keys()
        .find(|key| !VALID_KEYS.contains(&key.as_str()))
    {
        return Err(ApiError::bad_request(format!(
            "Invalid key {key} in request body"
        )));
    }

    let name = match path_name {
        Some(path_name) => {
            match object.get("name") {
                None => object.insert("name".into(), json!(path_name)),
                Some(Value::String(body_name)) if body_name == path_name => None,
                Some(_) => {
                    return Err(ApiError::bad_request("Node name mismatch."));
                }
            };
            path_name.to_string()
        }
        None => {
            let name = object
                .get("name")
                .ok_or_else(|| ApiError::bad_request("Field 'name' missing"))?;
            let name = name
                .as_str()
                .ok_or_else(|| ApiError::bad_request("Field 'name' invalid"))?;
            if !valid_name(name) {
                return Err(ApiError::bad_request("Field 'name' invalid"));
            }
            name.to_string()
        }
    };
    if !valid_name(&name) {
        return Err(ApiError::bad_request("Field 'name' invalid"));
    }
    object.insert("name".into(), json!(name));

    let environment = match object.get("chef_environment") {
        None | Some(Value::Null) => "_default".to_string(),
        Some(Value::String(value)) if valid_environment(value) => value.clone(),
        Some(_) => return Err(ApiError::bad_request("Field 'chef_environment' invalid")),
    };
    object.insert("chef_environment".into(), json!(environment));

    validate_fixed_field(object, "json_class", "Chef::Node")?;
    validate_fixed_field(object, "chef_type", "node")?;

    for key in ATTRIBUTE_KEYS {
        match object.get(key) {
            None | Some(Value::Null) => {
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
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items))
            if items
                .iter()
                .all(|item| item.as_str().is_some_and(valid_run_list_item)) =>
        {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect()
        }
        Some(_) => {
            return Err(ApiError::bad_request(
                "Field 'run_list' is not a valid run list",
            ));
        }
    };
    let normalized_run_list = normalize_run_list(run_list);
    object.insert("run_list".into(), json!(normalized_run_list));

    let policy_name = validate_optional_name(object, "policy_name")?;
    let policy_group = validate_optional_name(object, "policy_group")?;

    let serialized = serde_json::to_string(body).map_err(|_| ApiError::internal())?;
    Ok(NormalizedNode {
        name,
        environment,
        policy_name,
        policy_group,
        serialized,
    })
}

fn validate_fixed_field(
    object: &mut Map<String, Value>,
    key: &str,
    expected: &str,
) -> Result<(), ApiError> {
    match object.get(key) {
        None | Some(Value::Null) => {
            object.insert(key.into(), json!(expected));
            Ok(())
        }
        Some(Value::String(value)) if value == expected => Ok(()),
        Some(_) => Err(ApiError::bad_request(format!("Field '{key}' invalid"))),
    }
}

fn validate_optional_name(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, ApiError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if valid_policy_name(value) => Ok(Some(value.clone())),
        Some(_) => Err(ApiError::bad_request(format!("Field '{key}' invalid"))),
    }
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
}

fn valid_environment(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn valid_policy_name(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 255
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':'))
}

fn valid_run_list_item(item: &str) -> bool {
    if let Some(name) = item
        .strip_prefix("recipe[")
        .and_then(|i| i.strip_suffix(']'))
    {
        return valid_recipe(name);
    }
    if let Some(name) = item.strip_prefix("role[").and_then(|i| i.strip_suffix(']')) {
        return valid_recipe_name(name);
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
