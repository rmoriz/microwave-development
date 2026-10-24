use crate::error::ApiError;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

pub const MIN_API_VERSION: u8 = 0;
pub const MAX_API_VERSION: u8 = 2;
/// Minimum Chef client major version accepted by the API front end, matching
/// `min_version` in `nginx/habitat/config/dispatch.lua`.
pub const MIN_CHEF_VERSION: u32 = 10;

/// Upstream `oc_chef_wm`'s `max_request_size` default. Request bodies larger
/// than this are rejected with `413` before any JSON parsing happens.
pub const MAX_REQUEST_BODY: usize = 2_000_000;

pub async fn authenticate(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = request.uri().path().to_string();
    let requested_version = match parse_api_version(request.headers()) {
        Ok(version) => version,
        Err(value) => return version_error(&value),
    };

    if let Some(version) = request
        .headers()
        .get("x-chef-version")
        .and_then(|value| value.to_str().ok())
    {
        if !chef_version_supported(version, MIN_CHEF_VERSION) {
            return with_version_header(
                chef_version_error(version),
                &requested_version.to_string(),
                &requested_version.to_string(),
            );
        }
    }

    // The credential endpoints only accept POST. Upstream reports 405 before
    // attempting to authenticate an unsupported method, so let the router
    // produce the method-not-allowed response.
    if (path == "/authenticate_user" || path == "/system_recovery")
        && request.method() != axum::http::Method::POST
    {
        let response = next.run(request).await;
        return with_version_header(
            response,
            &requested_version.to_string(),
            &requested_version.to_string(),
        );
    }

    if path == "/_status"
        || path == "/server_api_version"
        || path == "/id"
        || path.starts_with("/id/")
        || path.starts_with("/bookshelf/")
    {
        let response = next.run(request).await;
        return with_version_header(
            response,
            &requested_version.to_string(),
            &requested_version.to_string(),
        );
    }

    match authenticate_signed(state, request, requested_version, next).await {
        Ok(response) => with_version_header(
            response,
            &requested_version.to_string(),
            &requested_version.to_string(),
        ),
        Err(error) => with_version_header(
            error.into_response(),
            &requested_version.to_string(),
            &requested_version.to_string(),
        ),
    }
}

async fn authenticate_signed(
    state: crate::AppState,
    request: axum::extract::Request,
    api_version: u8,
    next: axum::middleware::Next,
) -> Result<Response, ApiError> {
    let (parts, body) = request.into_parts();
    let body_bytes = axum::body::to_bytes(body, 16 * 1024 * 1024)
        .await
        .map_err(|_| ApiError::bad_request("request body is too large"))?;
    if matches!(parts.method.as_str(), "POST" | "PUT") && body_bytes.len() > MAX_REQUEST_BODY {
        return Err(ApiError::payload_too_large(format!(
            "JSON must be no more than {MAX_REQUEST_BODY} bytes."
        )));
    }

    if parts.method == axum::http::Method::POST {
        crate::users::validate_credentials_request(parts.uri.path(), &body_bytes)?;
    }

    let headers = &parts.headers;
    let sign = required_header(headers, "x-ops-sign")?;
    let user_id = required_header(headers, "x-ops-userid")?;
    let timestamp = required_header(headers, "x-ops-timestamp")?;
    let content_hash = required_header(headers, "x-ops-content-hash")?;
    let authorization = authorization_lines(headers)?;
    let signature =
        microwave_crypto::decode_signature(&authorization).map_err(|_| ApiError::unauthorized())?;
    let version =
        microwave_crypto::parse_sign_header(&sign).map_err(|_| ApiError::unauthorized())?;

    let auth_headers = microwave_crypto::AuthHeaders {
        version,
        user_id: user_id.clone(),
        timestamp: timestamp.clone(),
        content_hash,
        signature,
    };

    let path = parts.uri.path().to_string();
    let method = parts.method.as_str().to_string();
    let org = match organization_name_from_path(&path) {
        Some(name) => Some(
            state
                .store
                .find_org(&name)
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::not_found(format!("organization {name} not found")))?,
        ),
        None => None,
    };

    let principals = state
        .store
        .find_principals(org.as_ref().map(|org| org.id.as_str()), &user_id)
        .map_err(ApiError::from)?;
    if principals.is_empty() {
        return Err(ApiError::unauthorized());
    }

    let api_version_string = api_version.to_string();
    let canonical = microwave_crypto::CanonicalRequest {
        method: &method,
        path: &path,
        body: &body_bytes,
        user_id: &user_id,
        timestamp: &timestamp,
        api_version: Some(&api_version_string),
        version,
    };

    let mut authenticated = None;
    for principal in principals {
        let keys = state
            .store
            .find_principal_keys(&principal)
            .map_err(ApiError::from)?;
        for key in keys {
            if microwave_crypto::verify_request(&key.public_key, &auth_headers, &canonical).is_ok()
            {
                authenticated = Some(principal);
                break;
            }
        }
        if authenticated.is_some() {
            break;
        }
    }

    let principal = authenticated.ok_or_else(ApiError::unauthorized)?;
    let mut request = axum::extract::Request::from_parts(parts, axum::body::Body::from(body_bytes));
    request
        .extensions_mut()
        .insert(crate::state::ApiVersion(api_version));
    request
        .extensions_mut()
        .insert(crate::state::AuthContext { principal, org });

    Ok(next.run(request).await)
}

fn parse_api_version(headers: &HeaderMap) -> Result<u8, String> {
    match headers.get("x-ops-server-api-version") {
        None => Ok(0),
        Some(value) => {
            let value = value.to_str().map_err(|_| "invalid".to_string())?;
            let version: i16 = value.trim().parse().map_err(|_| value.trim().to_string())?;
            if (MIN_API_VERSION as i16..=MAX_API_VERSION as i16).contains(&version) {
                Ok(version as u8)
            } else {
                Err(value.trim().to_string())
            }
        }
    }
}

fn required_header(headers: &HeaderMap, name: &str) -> Result<String, ApiError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
        .ok_or_else(ApiError::unauthorized)
}

fn authorization_lines(headers: &HeaderMap) -> Result<Vec<&str>, ApiError> {
    let mut lines = Vec::new();
    for (name, value) in headers {
        let name = name.as_str();
        if let Some(index) = name.strip_prefix("x-ops-authorization-") {
            let index: usize = index.parse().map_err(|_| ApiError::unauthorized())?;
            let value = value.to_str().map_err(|_| ApiError::unauthorized())?;
            lines.push((index, value));
        }
    }
    lines.sort_by_key(|(index, _)| *index);
    Ok(lines.into_iter().map(|(_, value)| value).collect())
}

fn organization_name_from_path(path: &str) -> Option<String> {
    let mut parts = path.trim_matches('/').split('/');
    if parts.next() == Some("organizations") {
        parts
            .next()
            .filter(|part| !part.is_empty())
            .map(ToString::to_string)
    } else {
        None
    }
}

/// Match `routes.is_client_version_valid/2`: a version is accepted when its
/// major component is at or above the minimum, or when it is the legacy
/// `0.10.x` series.
fn chef_version_supported(version: &str, min_major: u32) -> bool {
    let mut parts = version.split('.');
    let Some(major) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
        return false;
    };
    let Some(minor) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
        return false;
    };
    // The upstream parser requires a third dot-delimited component.
    if parts.next().is_none() {
        return false;
    }
    major >= min_major || (major == 0 && minor == 10)
}

fn chef_version_error(version: &str) -> Response {
    let body = json!({
        "error": format!(
            "400 - Bad Request: Chef Client version {MIN_CHEF_VERSION} or higher required. Your version of Chef is {version}."
        )
    });
    (StatusCode::BAD_REQUEST, axum::Json(body)).into_response()
}

fn version_error(requested: &str) -> Response {
    let body = json!({
        "error": "invalid-x-ops-server-api-version",
        "message": format!("Specified version {requested} not supported"),
        "min_version": MIN_API_VERSION,
        "max_version": MAX_API_VERSION,
    });
    with_version_header(
        (StatusCode::NOT_ACCEPTABLE, axum::Json(body)).into_response(),
        requested,
        "-1",
    )
}

fn with_version_header(
    mut response: Response,
    requested: &str,
    response_version: &str,
) -> Response {
    let header = format!(
        r#"{{"min_version":"{}","max_version":"{}","request_version":"{}","response_version":"{}"}}"#,
        MIN_API_VERSION, MAX_API_VERSION, requested, response_version
    );
    if let Ok(value) = HeaderValue::from_str(&header) {
        response
            .headers_mut()
            .insert("x-ops-server-api-version", value);
    }
    response
}
