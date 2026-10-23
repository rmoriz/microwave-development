use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use microwave_authz::create_organization;
use serde_json::{json, Map, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/organizations", get(list_orgs).post(create_org))
        .route(
            "/organizations/{organization}",
            get(get_org).delete(delete_org),
        )
}

async fn list_orgs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let base = base_url(&headers, &state);
    let mut orgs = Map::new();
    for org in state.store.list_orgs().map_err(ApiError::from)? {
        orgs.insert(
            org.name.clone(),
            json!(format!("{base}/organizations/{}", org.name)),
        );
    }
    Ok(Json(Value::Object(orgs)))
}

async fn create_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let name = body
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::bad_request("name is required"))?;
    let full_name = body
        .get("full_name")
        .and_then(Value::as_str)
        .unwrap_or(name);

    let org = create_organization(
        &state.store,
        name,
        full_name,
        &auth.principal.actor_id,
        &auth.principal.authz_id,
    )
    .map_err(ApiError::from)?;

    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}", org.name),
            "name": org.name,
            "guid": org.id
        })),
    ))
}

async fn get_org(
    State(state): State<AppState>,
    Path(organization): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let org = state
        .store
        .find_org(&organization)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("organization {organization}")))?;
    Ok(Json(json!({
        "name": org.name,
        "full_name": org.full_name,
        "guid": org.id
    })))
}

async fn delete_org(
    State(state): State<AppState>,
    Path(organization): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let org = state
        .store
        .find_org(&organization)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("organization {organization}")))?;
    state.store.delete_org(&org.id).map_err(ApiError::from)?;
    Ok(Json(json!({})))
}

fn base_url(headers: &HeaderMap, state: &AppState) -> String {
    let host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get("host"))
        .and_then(|value| value.to_str().ok())
        .unwrap_or(state.config.server.bind.as_str());
    let proto = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("http");
    format!("{proto}://{host}")
}
