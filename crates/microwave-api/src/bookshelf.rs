use crate::{ApiError, AppState};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use chrono::{DateTime, Utc};
use microwave_objectstore::{
    bookshelf_token_matches, checksum_md5_base64, md5_hex, ObjectMetadata, ObjectStoreError,
    DEFAULT_PRESIGN_EXPIRY_SECONDS,
};
use serde::Deserialize;
use std::time::SystemTime;

/// Upload cap matching the upstream nginx `client_max_body_size 250m`.
///
/// The `ObjectStore` trait takes a byte slice, so the upload body is buffered
/// in memory and a single request can occupy up to this much RAM. The explicit
/// limit keeps that bound tight instead of inheriting axum's 2 MiB default;
/// larger requests are rejected with `413 Payload Too Large` before the
/// handler runs. Streaming the body into the local store would require a
/// file-based object-store API, which this layer does not expose yet.
pub const MAX_UPLOAD_BYTES: usize = 250 * 1024 * 1024;

/// Cache freshness for blob responses. Upstream emits the presigned URL's
/// expiry as `Cache-Control: max-age=...`; local tokens do not carry an
/// expiry, so the default presign lifetime is used.
pub const CACHE_CONTROL_MAX_AGE: u64 = DEFAULT_PRESIGN_EXPIRY_SECONDS;

#[derive(Debug, Deserialize)]
struct TokenQuery {
    token: String,
}

fn authorized(
    state: &AppState,
    method: &str,
    organization: &str,
    checksum: &str,
    token: &str,
) -> bool {
    let secret = state
        .config
        .object_store
        .secret
        .as_deref()
        .unwrap_or_default();
    bookshelf_token_matches(secret, method, organization, checksum, token)
}

pub fn routes() -> Router<AppState> {
    routes_with_limit(MAX_UPLOAD_BYTES)
}

/// Build the bookshelf routes with an explicit upload cap.
///
/// Exposed so tests can exercise the body-limit behavior without sending a
/// quarter-gigabyte request.
pub fn routes_with_limit(limit: usize) -> Router<AppState> {
    Router::new()
        .route(
            "/bookshelf/organizations/{organization}/checksums/{checksum}",
            get(download).put(upload).head(head).delete(delete_object),
        )
        .layer(axum::extract::DefaultBodyLimit::max(limit))
}

async fn download(
    State(state): State<AppState>,
    Path((organization, checksum)): Path<(String, String)>,
    Query(query): Query<TokenQuery>,
) -> Result<Response, ApiError> {
    if !authorized(&state, "GET", &organization, &checksum, &query.token) {
        return Err(ApiError::unauthorized());
    }
    let data = match state.object_store.get(&organization, &checksum) {
        Ok(data) => data,
        Err(ObjectStoreError::NotFound) => return Err(ApiError::not_found("checksum not found")),
        Err(_) => return Err(ApiError::internal()),
    };
    let metadata = object_metadata(&state, &organization, &checksum);
    Ok((object_headers(&checksum, metadata), data).into_response())
}

async fn head(
    State(state): State<AppState>,
    Path((organization, checksum)): Path<(String, String)>,
    Query(query): Query<TokenQuery>,
) -> Result<Response, ApiError> {
    if !authorized(&state, "GET", &organization, &checksum, &query.token) {
        return Err(ApiError::unauthorized());
    }
    match object_metadata(&state, &organization, &checksum) {
        Some(metadata) => {
            Ok((object_headers(&checksum, Some(metadata)), StatusCode::OK).into_response())
        }
        None => Ok(StatusCode::NOT_FOUND.into_response()),
    }
}

async fn upload(
    State(state): State<AppState>,
    Path((organization, checksum)): Path<(String, String)>,
    Query(query): Query<TokenQuery>,
    body: Bytes,
) -> Result<Response, ApiError> {
    if !authorized(&state, "PUT", &organization, &checksum, &query.token) {
        return Err(ApiError::unauthorized());
    }
    if md5_hex(&body) != checksum {
        return Ok(StatusCode::NOT_ACCEPTABLE.into_response());
    }
    match state.object_store.put(&organization, &checksum, &body) {
        Ok(()) => {
            let mut headers = HeaderMap::new();
            if let Some(etag) = etag_value(&checksum) {
                headers.insert(header::ETAG, etag);
            }
            Ok((headers, StatusCode::ACCEPTED).into_response())
        }
        Err(ObjectStoreError::ChecksumMismatch) => Ok(StatusCode::NOT_ACCEPTABLE.into_response()),
        Err(_) => Err(ApiError::internal()),
    }
}

async fn delete_object(
    State(state): State<AppState>,
    Path((organization, checksum)): Path<(String, String)>,
    Query(query): Query<TokenQuery>,
) -> Result<Response, ApiError> {
    if !authorized(&state, "DELETE", &organization, &checksum, &query.token) {
        return Err(ApiError::unauthorized());
    }
    state
        .object_store
        .delete(&organization, &checksum)
        .map_err(|_| ApiError::internal())?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Fetch metadata, falling back to a plain existence check for backends that
/// cannot report it. `None` means the object is absent.
fn object_metadata(state: &AppState, organization: &str, checksum: &str) -> Option<ObjectMetadata> {
    match state.object_store.metadata(organization, checksum) {
        Ok(Some(metadata)) => Some(metadata),
        Ok(None) | Err(_) if state.object_store.exists(organization, checksum) => {
            Some(ObjectMetadata::default())
        }
        _ => None,
    }
}

fn object_headers(checksum: &str, metadata: Option<ObjectMetadata>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    if let Some(etag) = etag_value(checksum) {
        headers.insert(header::ETAG, etag);
    }
    if let Some(modified) = metadata.and_then(|metadata| metadata.modified) {
        if let Ok(value) = HeaderValue::from_str(&http_date(modified)) {
            headers.insert(header::LAST_MODIFIED, value);
        }
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_str(&format!("max-age={CACHE_CONTROL_MAX_AGE}"))
            .expect("constant cache-control value is a valid header"),
    );
    headers
}

/// S3-style quoted ETag built from the checksum's MD5 digest.
fn etag_value(checksum: &str) -> Option<HeaderValue> {
    let digest = checksum_md5_base64(checksum)?;
    HeaderValue::from_str(&format!("\"{digest}\"")).ok()
}

fn http_date(time: SystemTime) -> String {
    let time: DateTime<Utc> = time.into();
    time.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

pub fn checksums_from_body(body: &serde_json::Value) -> Result<Vec<String>, ApiError> {
    let checksums = body
        .get("checksums")
        .ok_or_else(|| ApiError::bad_request("Field 'checksums' missing"))?
        .as_object()
        .ok_or_else(|| ApiError::bad_request("Bad checksums!"))?;
    if checksums.is_empty() {
        return Err(ApiError::bad_request("Bad checksums!"));
    }
    let mut result = Vec::with_capacity(checksums.len());
    for (checksum, value) in checksums {
        if !value.is_null() || !is_md5_hex(checksum) {
            return Err(ApiError::bad_request("Bad checksums!"));
        }
        result.push(checksum.clone());
    }
    Ok(result)
}

fn is_md5_hex(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn checksum_map(
    checks: Vec<microwave_store::SandboxCheck>,
    state: &AppState,
    org_id: &str,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for check in checks {
        let entry = if check.needs_upload {
            serde_json::json!({
                "needs_upload": true,
                "url": state
                    .object_store
                    .presigned_url("PUT", org_id, &check.checksum),
            })
        } else {
            serde_json::json!({ "needs_upload": false })
        };
        map.insert(check.checksum, entry);
    }
    serde_json::Value::Object(map)
}
