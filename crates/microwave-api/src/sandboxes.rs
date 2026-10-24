use crate::bookshelf::{checksum_map, checksums_from_body};
use crate::helpers::{base_url, container_authz, organization, require_permission};
use crate::{ApiError, AppState, AuthContext};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Extension, Json, Router};
use microwave_store::Permission;
use serde_json::{json, Value};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/organizations/{organization}/sandboxes",
            post(create_sandbox),
        )
        .route(
            "/organizations/{organization}/sandboxes/{sandbox}",
            axum::routing::put(commit_sandbox),
        )
}

async fn create_sandbox(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "sandboxes")?;
    require_permission(&state, &auth, &container, Permission::Create)?;

    let checksums = checksums_from_body(&body)?;
    let (sandbox_id, checks) = state
        .store
        .create_sandbox(&org.id, &checksums)
        .map_err(ApiError::from)?;
    let base = base_url(&headers, &state);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "uri": format!("{base}/organizations/{}/sandboxes/{sandbox_id}", org.name),
            "sandbox_id": sandbox_id,
            "checksums": checksum_map(checks, &state, &org.id)
        })),
    ))
}

async fn commit_sandbox(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Path((_organization, sandbox_id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let org = organization(&auth)?;
    let container = container_authz(&state, org, "sandboxes")?;
    require_permission(&state, &auth, &container, Permission::Create)?;

    if body.get("is_completed").and_then(Value::as_bool) != Some(true) {
        return Err(ApiError::bad_request(
            "JSON body must contain is_completed: true",
        ));
    }
    if !state
        .store
        .sandbox_exists(&org.id, &sandbox_id)
        .map_err(ApiError::from)?
    {
        return Err(ApiError::not_found(format!(
            "No such sandbox '{sandbox_id}'."
        )));
    }

    let created_at = state
        .store
        .sandbox_created_at(&org.id, &sandbox_id)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError::not_found(format!("No such sandbox '{sandbox_id}'.")))?;
    let checksums = state
        .store
        .sandbox_checksums(&org.id, &sandbox_id)
        .map_err(ApiError::from)?;
    let missing: Vec<&String> = checksums
        .iter()
        .filter(|checksum| !state.object_store.exists(&org.id, checksum))
        .collect();
    if !missing.is_empty() {
        return Err(ApiError::service_unavailable(format!(
            "Cannot update sandbox {sandbox_id}: the following checksums have not been uploaded: {}",
            missing
                .iter()
                .map(|checksum| checksum.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }

    state
        .store
        .commit_sandbox(&org.id, &sandbox_id)
        .map_err(ApiError::from)?;
    Ok(Json(json!({
        "guid": sandbox_id,
        "name": sandbox_id,
        "checksums": checksums,
        "create_time": upstream_timestamp(&created_at),
        "is_completed": true,
    })))
}

/// Erchef timestamps use an explicit `+00:00` offset; `Utc::now()` yields `Z`.
fn upstream_timestamp(value: &str) -> String {
    match value.strip_suffix('Z') {
        Some(stripped) => format!("{stripped}+00:00"),
        None => value.to_string(),
    }
}
