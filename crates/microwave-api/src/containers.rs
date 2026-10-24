use crate::helpers::{base_url, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::{Container, Org, Permission};
use serde_json::{json, Map, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/containers",
            get(list_containers).post(create_container),
        )
        .route(
            "/organizations/{organization}/containers/{container}",
            get(get_container).delete(delete_container),
        )
}

async fn list_containers(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "containers")?;
    require_permission(&state, &auth, &container, Permission::Read)?;
    let base = base_url(&headers, &state);
    let mut containers = Map::new();
    for container in state
        .store
        .list_containers(&org.id)
        .map_err(ApiError::from)?
    {
        containers.insert(
            container.name.clone(),
            json!(format!(
                "{base}/organizations/{}/containers/{}",
                org.name, container.name
            )),
        );
    }
    Ok(Json(Value::Object(containers)))
}

async fn create_container(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "containers")?;
    require_permission(&state, &auth, &container, Permission::Create)?;
    let name = container_name(&body)?;

    let container = state
        .store
        .create_container_for_org_with_acl(&org.id, &name, &auth.principal.authz_id)
        .map_err(ApiError::from)?;
    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!(
                "{base}/organizations/{}/containers/{}",
                org.name, container.name
            )
        })),
    ))
}

async fn get_container(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, container_name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = find_container(&state, org, &container_name)?;
    require_permission(&state, &auth, &container.authz_id, Permission::Read)?;
    Ok(Json(container_json(&container)))
}

async fn delete_container(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, container_name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = find_container(&state, org, &container_name)?;
    require_permission(&state, &auth, &container.authz_id, Permission::Delete)?;
    if microwave_authz::DEFAULT_CONTAINERS.contains(&container.name.as_str()) {
        return Err(ApiError::forbidden_with(
            "default organization containers cannot be deleted",
        ));
    }
    let response = container_json(&container);
    state
        .store
        .delete_container(&org.id, &container_name)
        .map_err(ApiError::from)?;
    Ok(Json(response))
}

fn container_authz(state: &AppState, org: &Org, name: &str) -> Result<String, ApiError> {
    Ok(find_container(state, org, name)?.authz_id)
}

fn find_container(state: &AppState, org: &Org, name: &str) -> Result<Container, ApiError> {
    state
        .store
        .find_container(&org.id, name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("container {name}")))
}

fn container_name(body: &Value) -> Result<String, ApiError> {
    let name = body
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| body.get("containername").and_then(Value::as_str))
        .unwrap_or_default();
    let valid = !name.is_empty()
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character == '_'
                || character == '-'
                || character == '.'
        });
    if valid {
        Ok(name.to_string())
    } else {
        Err(ApiError::bad_request(
            "Malformed container name. Must only contain A-Z, a-z, 0-9, _, -, or .",
        ))
    }
}

fn container_json(container: &Container) -> Value {
    json!({
        "containername": container.name,
        "containerpath": container.name,
    })
}
