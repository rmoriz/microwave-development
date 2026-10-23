use crate::{ApiError, AppState};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use microwave_objectstore::md5_hex;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct TokenQuery {
    token: String,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/bookshelf/organizations/{organization}/checksums/{checksum}",
            get(download).put(upload).head(head).delete(delete_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
}

async fn download(
    State(state): State<AppState>,
    Path((organization, checksum)): Path<(String, String)>,
    Query(query): Query<TokenQuery>,
) -> Result<Response, ApiError> {
    if !state
        .object_store
        .verify("GET", &organization, &checksum, &query.token)
    {
        return Err(ApiError::unauthorized());
    }
    let data = state
        .object_store
        .get(&organization, &checksum)
        .map_err(|_| ApiError::not_found("checksum not found"))?;
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], data).into_response())
}

async fn head(
    State(state): State<AppState>,
    Path((organization, checksum)): Path<(String, String)>,
    Query(query): Query<TokenQuery>,
) -> Result<StatusCode, ApiError> {
    if !state
        .object_store
        .verify("GET", &organization, &checksum, &query.token)
    {
        return Err(ApiError::unauthorized());
    }
    if state.object_store.exists(&organization, &checksum) {
        Ok(StatusCode::OK)
    } else {
        Ok(StatusCode::NOT_FOUND)
    }
}

async fn upload(
    State(state): State<AppState>,
    Path((organization, checksum)): Path<(String, String)>,
    Query(query): Query<TokenQuery>,
    body: Bytes,
) -> Result<StatusCode, ApiError> {
    if !state
        .object_store
        .verify("PUT", &organization, &checksum, &query.token)
    {
        return Err(ApiError::unauthorized());
    }
    if md5_hex(&body) != checksum {
        return Err(ApiError::bad_request("content checksum mismatch"));
    }
    state
        .object_store
        .put(&organization, &checksum, &body)
        .map_err(|_| ApiError::internal())?;
    Ok(StatusCode::OK)
}

async fn delete_object(
    State(state): State<AppState>,
    Path((organization, checksum)): Path<(String, String)>,
    Query(query): Query<TokenQuery>,
) -> Result<StatusCode, ApiError> {
    if !state
        .object_store
        .verify("DELETE", &organization, &checksum, &query.token)
    {
        return Err(ApiError::unauthorized());
    }
    state
        .object_store
        .delete(&organization, &checksum)
        .map_err(|_| ApiError::internal())?;
    Ok(StatusCode::OK)
}

pub fn checksums_from_body(body: &serde_json::Value) -> Result<Vec<String>, ApiError> {
    let checksums = body
        .get("checksums")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| ApiError::bad_request("checksums is required"))?;
    Ok(checksums.keys().cloned().collect())
}

pub fn checksum_map(
    checks: Vec<microwave_store::SandboxCheck>,
    state: &AppState,
    org_id: &str,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for check in checks {
        let url = state
            .object_store
            .presigned_url("PUT", org_id, &check.checksum);
        map.insert(
            check.checksum,
            serde_json::json!({
                "needs_upload": check.needs_upload,
                "url": url
            }),
        );
    }
    serde_json::Value::Object(map)
}
