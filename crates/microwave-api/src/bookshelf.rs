use crate::{ApiError, AppState};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use chrono::{DateTime, Utc};
use microwave_objectstore::{
    bookshelf_token_expiry_valid, bookshelf_token_matches_at, checksum_md5_base64, md5_hex,
    unix_timestamp, ObjectMetadata, ObjectStoreError, DEFAULT_PRESIGN_EXPIRY_SECONDS,
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
/// expiry as `Cache-Control: max-age=...`; local tokens default to the same
/// lifetime, so the default presign lifetime is used here.
pub const CACHE_CONTROL_MAX_AGE: u64 = DEFAULT_PRESIGN_EXPIRY_SECONDS;

#[derive(Debug, Deserialize)]
struct TokenQuery {
    token: Option<String>,
    exp: Option<u64>,
}

/// Validate a local bookshelf token.
///
/// A missing or temporally invalid `exp` is a `403`; a well-formed token with a
/// bad signature remains a `401`, matching the historical behavior.
fn authorized(
    state: &AppState,
    method: &str,
    organization: &str,
    checksum: &str,
    query: &TokenQuery,
) -> Result<(), ApiError> {
    let Some(token) = query.token.as_deref() else {
        return Err(ApiError::forbidden());
    };
    let Some(expires_at) = query.exp else {
        return Err(ApiError::forbidden());
    };
    let now = unix_timestamp();
    if !bookshelf_token_expiry_valid(expires_at, now) {
        return Err(ApiError::forbidden());
    }

    let secret = state
        .config
        .object_store
        .secret
        .as_deref()
        .unwrap_or_default();
    if bookshelf_token_matches_at(
        secret,
        method,
        organization,
        checksum,
        expires_at,
        token,
        now,
    ) {
        Ok(())
    } else {
        Err(ApiError::unauthorized())
    }
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
    request_headers: HeaderMap,
) -> Result<Response, ApiError> {
    authorized(&state, "GET", &organization, &checksum, &query)?;

    // The object-store trait returns a complete `Vec<u8>`, so the download is
    // buffered. Range requests slice the same buffer through `Bytes` instead
    // of copying the requested window, which keeps peak memory at one object
    // rather than one object plus the response body.
    let data = match state.object_store.get(&organization, &checksum) {
        Ok(data) => Bytes::from(data),
        Err(ObjectStoreError::NotFound) => return Err(ApiError::not_found("checksum not found")),
        Err(_) => return Err(ApiError::internal()),
    };
    let metadata = object_metadata(&state, &organization, &checksum);
    let headers = object_headers(&checksum, metadata);
    let total = data.len() as u64;

    if let Some(range) = request_headers.get(header::RANGE) {
        let parsed = range
            .to_str()
            .ok()
            .and_then(|value| parse_range(value, total).ok());
        return Ok(match parsed {
            Some((start, end)) => partial_content(headers, data, total, start, end),
            None => range_not_satisfiable(total),
        });
    }

    let mut headers = headers;
    headers.insert(header::CONTENT_LENGTH, content_length(total));
    Ok((headers, data).into_response())
}

async fn head(
    State(state): State<AppState>,
    Path((organization, checksum)): Path<(String, String)>,
    Query(query): Query<TokenQuery>,
) -> Result<Response, ApiError> {
    authorized(&state, "GET", &organization, &checksum, &query)?;
    match object_metadata(&state, &organization, &checksum) {
        Some(metadata) => {
            let mut headers = object_headers(&checksum, Some(metadata));
            headers.insert(header::CONTENT_LENGTH, content_length(metadata.len));
            Ok((headers, StatusCode::OK).into_response())
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
    authorized(&state, "PUT", &organization, &checksum, &query)?;
    if md5_hex(&body) != checksum {
        return Ok(StatusCode::NOT_ACCEPTABLE.into_response());
    }
    match state.object_store.put(&organization, &checksum, &body) {
        Ok(()) => {
            let mut headers = HeaderMap::new();
            if let Some(etag) = etag_value(&checksum) {
                headers.insert(header::ETAG, etag);
            }
            Ok((headers, StatusCode::OK).into_response())
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
    authorized(&state, "DELETE", &organization, &checksum, &query)?;
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
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
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

/// Parse a single `Range` header value against a representation of `len`
/// bytes.
///
/// Returns the inclusive `(start, end)` pair. Multi-range requests and
/// malformed or unsatisfiable ranges return `Err(())`, which the handler maps
/// to `416` with `Content-Range: bytes */len`.
fn parse_range(value: &str, len: u64) -> Result<(u64, u64), ()> {
    let spec = value.trim().strip_prefix("bytes=").ok_or(())?;
    if spec.contains(',') || len == 0 {
        return Err(());
    }
    let (start_text, end_text) = spec.split_once('-').ok_or(())?;

    if start_text.is_empty() {
        // Suffix range: `bytes=-N` returns the final N bytes. A suffix larger
        // than the object is clamped to the whole object.
        let suffix: u64 = end_text.parse().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        return Ok((len.saturating_sub(suffix), len - 1));
    }

    let start: u64 = start_text.parse().map_err(|_| ())?;
    if start >= len {
        return Err(());
    }
    let end = if end_text.is_empty() {
        len - 1
    } else {
        end_text.parse::<u64>().map_err(|_| ())?.min(len - 1)
    };
    if end < start {
        return Err(());
    }
    Ok((start, end))
}

fn partial_content(
    mut headers: HeaderMap,
    data: Bytes,
    total: u64,
    start: u64,
    end: u64,
) -> Response {
    let length = end - start + 1;
    headers.insert(header::CONTENT_LENGTH, content_length(length));
    headers.insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes {start}-{end}/{total}"))
            .expect("content-range is a valid header"),
    );
    let body = data.slice(start as usize..=end as usize);
    (StatusCode::PARTIAL_CONTENT, headers, body).into_response()
}

fn range_not_satisfiable(total: u64) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{total}"))
            .expect("content-range is a valid header"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
    (StatusCode::RANGE_NOT_SATISFIABLE, headers).into_response()
}

fn content_length(value: u64) -> HeaderValue {
    HeaderValue::from_str(&value.to_string()).expect("decimal length is a valid header")
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
