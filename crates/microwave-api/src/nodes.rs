use crate::helpers::{base_url, container_authz, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::{NewNode, Permission};
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/nodes",
            get(list_nodes).post(create_node),
        )
        .route(
            "/organizations/{organization}/nodes/{node}",
            get(get_node).put(update_node).delete(delete_node),
        )
}

async fn list_nodes(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let nodes_container = container_authz(&state, org, "nodes")?;
    require_permission(&state, &auth, &nodes_container, Permission::Read)?;

    let base = base_url(&headers, &state);
    let mut nodes = serde_json::Map::new();
    for node in state.store.list_nodes(&org.id).map_err(ApiError::from)? {
        nodes.insert(
            node.name.clone(),
            json!(format!(
                "{base}/organizations/{}/nodes/{}",
                org.name, node.name
            )),
        );
    }
    Ok(Json(Value::Object(nodes)))
}

async fn create_node(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let nodes_container = container_authz(&state, org, "nodes")?;
    require_permission(&state, &auth, &nodes_container, Permission::Create)?;

    let normalized = normalize_node(&mut body)?;
    state
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
        .map_err(ApiError::from)?;

    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}/nodes/{}", org.name, normalized.name)
        })),
    ))
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
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("node {name}")))?;
    require_permission(&state, &auth, &node.authz_id, Permission::Read)?;
    let value = serde_json::from_str(&node.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
}

async fn update_node(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let existing = state
        .store
        .find_node(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("node {name}")))?;
    require_permission(&state, &auth, &existing.authz_id, Permission::Update)?;

    let normalized = normalize_node(&mut body)?;
    if normalized.name != name {
        return Err(ApiError::bad_request(
            "node name does not match request path",
        ));
    }

    let node = state
        .store
        .update_node(
            &org.id,
            &name,
            &normalized.serialized,
            &normalized.environment,
            normalized.policy_name.as_deref(),
            normalized.policy_group.as_deref(),
        )
        .map_err(ApiError::from)?;
    let value = serde_json::from_str(&node.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
}

async fn delete_node(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let node = state
        .store
        .find_node(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("node {name}")))?;
    require_permission(&state, &auth, &node.authz_id, Permission::Delete)?;
    state
        .store
        .delete_node(&org.id, &name)
        .map_err(ApiError::from)?;
    Ok(Json(json!({})))
}

struct NormalizedNode {
    name: String,
    environment: String,
    policy_name: Option<String>,
    policy_group: Option<String>,
    serialized: String,
}

fn normalize_node(body: &mut Value) -> Result<NormalizedNode, ApiError> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("node name is required"))?
        .to_string();

    if body.get("chef_environment").is_none() {
        body["chef_environment"] = json!("_default");
    }
    body["name"] = json!(name);
    body["json_class"] = json!("Chef::Node");
    body["chef_type"] = json!("node");

    let environment = body
        .get("chef_environment")
        .and_then(Value::as_str)
        .unwrap_or("_default")
        .to_string();
    let policy_name = body
        .get("policy_name")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let policy_group = body
        .get("policy_group")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let serialized = serde_json::to_string(body).map_err(|_| ApiError::internal())?;
    Ok(NormalizedNode {
        name,
        environment,
        policy_name,
        policy_group,
        serialized,
    })
}
