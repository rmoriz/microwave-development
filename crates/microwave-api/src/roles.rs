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
