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
    Json(mut body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "roles").await?;
    require_permission(&state, &auth, &container, Permission::Create).await?;

    let name = normalize_role(&mut body)?;
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
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}/roles/{name}", org.name)
        })),
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
    let value = serde_json::from_str(&role.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
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
    serde_json::from_str(&role.serialized_object).map_err(|_| ApiError::internal())
}

async fn update_role(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, name)): Path<(String, String)>,
    Json(mut body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let role = state
        .store
        .find_role(&org.id, &name)
        .await
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("role {name}")))?;
    require_permission(&state, &auth, &role.authz_id, Permission::Update).await?;

    let body_name = normalize_role(&mut body)?;
    if body_name != name {
        return Err(ApiError::bad_request(
            "role name does not match request path",
        ));
    }
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
    let value =
        serde_json::from_str(&updated.serialized_object).map_err(|_| ApiError::internal())?;
    Ok(Json(value))
}

async fn delete_role(
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
    require_permission(&state, &auth, &role.authz_id, Permission::Delete).await?;
    state
        .store
        .delete_role(&org.id, &name)
        .await
        .map_err(ApiError::from)?;
    crate::search::index::delete_document(&state, &org.id, &role.id).await;
    Ok(Json(json!({})))
}

fn normalize_role(body: &mut Value) -> Result<String, ApiError> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("role name is required"))?
        .to_string();
    body["name"] = json!(name);
    body["json_class"] = json!("Chef::Role");
    body["chef_type"] = json!("role");
    if body.get("run_list").is_none() {
        body["run_list"] = json!([]);
    }
    if body.get("default_attributes").is_none() {
        body["default_attributes"] = json!({});
    }
    if body.get("override_attributes").is_none() {
        body["override_attributes"] = json!({});
    }
    Ok(name)
}
