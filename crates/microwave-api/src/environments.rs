use crate::helpers::{base_url, container_authz, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_store::Permission;
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/environments",
            get(list_environments).post(create_environment),
        )
        .route(
            "/organizations/{organization}/environments/{environment}",
            get(get_environment)
                .put(update_environment)
                .delete(delete_environment),
        )
}

async fn list_environments(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "environments")?;
    require_permission(&state, &auth, &container, Permission::Read)?;

    let base = base_url(&headers, &state);
    let mut environments = serde_json::Map::new();
    for environment in state
        .store
        .list_environments(&org.id)
        .map_err(ApiError::from)?
    {
        environments.insert(
            environment.name.clone(),
            json!(format!(
                "{base}/organizations/{}/environments/{}",
                org.name, environment.name
            )),
        );
    }
    Ok(Json(Value::Object(environments)))
}

async fn create_environment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "environments")?;
    require_permission(&state, &auth, &container, Permission::Create)?;

    let name = normalize_environment(&mut body)?;
    state
        .store
        .create_environment(
            &org.id,
            &name,
            &serde_json::to_string(&body).map_err(|_| ApiError::internal())?,
            &auth.principal.authz_id,
        )
        .map_err(ApiError::from)?;

    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}/environments/{name}", org.name)
        })),
    ))
}

async fn get_environment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let environment = state
        .store
        .find_environment(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("environment {name}")))?;
    require_permission(&state, &auth, &environment.authz_id, Permission::Read)?;
    let value =
        serde_json::from_str(&environment.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
}

async fn update_environment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    protect_default(&name)?;
    let org = organization(&auth)?;
    let environment = state
        .store
        .find_environment(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("environment {name}")))?;
    require_permission(&state, &auth, &environment.authz_id, Permission::Update)?;

    let body_name = normalize_environment(&mut body)?;
    if body_name != name {
        return Err(ApiError::bad_request(
            "environment name does not match request path",
        ));
    }
    let updated = state
        .store
        .update_environment(
            &org.id,
            &name,
            &serde_json::to_string(&body).map_err(|_| ApiError::internal())?,
        )
        .map_err(ApiError::from)?;
    let value =
        serde_json::from_str(&updated.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
}

async fn delete_environment(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    protect_default(&name)?;
    let org = organization(&auth)?;
    let environment = state
        .store
        .find_environment(&org.id, &name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("environment {name}")))?;
    require_permission(&state, &auth, &environment.authz_id, Permission::Delete)?;
    state
        .store
        .delete_environment(&org.id, &name)
        .map_err(ApiError::from)?;
    Ok(Json(json!({})))
}

fn protect_default(name: &str) -> Result<(), ApiError> {
    if name == "_default" {
        Err(ApiError::method_not_allowed(
            "The '_default' environment cannot be modified.",
        ))
    } else {
        Ok(())
    }
}

fn normalize_environment(body: &mut Value) -> Result<String, ApiError> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("environment name is required"))?
        .to_string();
    body["name"] = json!(name);
    body["json_class"] = json!("Chef::Environment");
    body["chef_type"] = json!("environment");
    if body.get("cookbook_versions").is_none() {
        body["cookbook_versions"] = json!({});
    }
    if body.get("default_attributes").is_none() {
        body["default_attributes"] = json!({});
    }
    if body.get("override_attributes").is_none() {
        body["override_attributes"] = json!({});
    }
    Ok(name)
}
