use crate::error::ApiError;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use microwave_store::ActorType;
use serde_json::json;

pub const MIN_API_VERSION: u8 = 0;
pub const MAX_API_VERSION: u8 = 2;
/// Minimum Chef client major version accepted by the API front end, matching
/// `min_version` in `nginx/habitat/config/dispatch.lua`.
pub const MIN_CHEF_VERSION: u32 = 10;

/// Upstream `oc_chef_wm`'s `max_request_size` default. Request bodies larger
/// than this are rejected with `413` before any JSON parsing happens.
pub const MAX_REQUEST_SIZE: usize = 2_000_000;
pub const MAX_REQUEST_BODY: usize = MAX_REQUEST_SIZE;

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

    // The org-scoped user key resources are read-only. Upstream reports 405
    // before attempting authentication, so a request with an unsupported
    // method must not fail signature verification first.
    if is_org_user_keys_path(&path) && request.method() != axum::http::Method::GET {
        let response = next.run(request).await;
        return with_version_header(
            response,
            &requested_version.to_string(),
            &requested_version.to_string(),
        );
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

    if is_principal_path(&path) {
        // The principal resource is unauthenticated upstream but still needs
        // the negotiated API version to choose the v0 vs v1+ response shape.
        let mut request = request;
        request
            .extensions_mut()
            .insert(crate::state::ApiVersion(requested_version));
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
    let body_bytes = axum::body::to_bytes(body, MAX_REQUEST_SIZE)
        .await
        .map_err(|_| ApiError::payload_too_large("JSON must be no more than 2000000 bytes."))?;
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
    let signature = microwave_crypto::decode_signature(&authorization)
        .map_err(|_| authentication_failure(&user_id))?;
    let version =
        microwave_crypto::parse_sign_header(&sign).map_err(|_| authentication_failure(&user_id))?;

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
                .await
                .map_err(ApiError::from)?
                .ok_or_else(|| ApiError::not_found(format!("organization {name} not found")))?,
        ),
        None => None,
    };

    let principals = state
        .store
        .find_principals(org.as_ref().map(|org| org.id.as_str()), &user_id)
        .await
        .map_err(ApiError::from)?;
    if principals.is_empty() {
        return Err(authentication_failure(&user_id));
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
    'principals: for principal in principals {
        let keys = state
            .store
            .find_principal_keys(&principal)
            .await
            .map_err(ApiError::from)?;
        for key in keys {
            // Expired keys still appear in the keys API but may not be used to
            // sign requests.
            if key.expired {
                continue;
            }
            if microwave_crypto::verify_request(&key.public_key, &auth_headers, &canonical).is_ok()
            {
                authenticated = Some((principal, key.key_name));
                break 'principals;
            }
        }
    }

    // Web requests may be signed with the webui key while naming the user to
    // impersonate. In this harness the webui key is the superuser key.
    if authenticated.is_none()
        && headers
            .get("x-ops-request-source")
            .and_then(|value| value.to_str().ok())
            == Some("web")
    {
        authenticated =
            impersonated_principal(&state, org.as_ref(), &user_id, &auth_headers, &canonical)
                .await?;
    }

    let (principal, key_name) = authenticated.ok_or_else(|| authentication_failure(&user_id))?;
    if let Some(org) = &org {
        if principal.actor_type == ActorType::User
            && principal.name != "pivotal"
            && !state
                .store
                .is_user_in_org(&org.id, &principal.actor_id)
                .await
                .map_err(ApiError::from)?
        {
            return Err(ApiError::forbidden_with(format!(
                "'{user_id}' not associated with organization '{}'",
                org.name
            )));
        }
    }
    let mut request = axum::extract::Request::from_parts(parts, axum::body::Body::from(body_bytes));
    request
        .extensions_mut()
        .insert(crate::state::ApiVersion(api_version));
    request.extensions_mut().insert(crate::state::AuthContext {
        principal,
        org,
        key_name: Some(key_name),
    });

    Ok(next.run(request).await)
}

/// Authenticate a webui-signed impersonation request. The request names the
/// user to become in `X-Ops-Userid`; the signature must verify with the
/// platform superuser's key.
async fn impersonated_principal(
    state: &crate::AppState,
    org: Option<&microwave_store::Org>,
    user_id: &str,
    auth_headers: &microwave_crypto::AuthHeaders,
    canonical: &microwave_crypto::CanonicalRequest<'_>,
) -> Result<Option<(microwave_store::Principal, String)>, ApiError> {
    let Some(superuser) = state
        .store
        .find_user("pivotal")
        .await
        .map_err(ApiError::from)?
    else {
        return Ok(None);
    };
    let keys = state
        .store
        .find_actor_keys(&superuser.id, ActorType::User)
        .await
        .map_err(ApiError::from)?;
    let mut verified_with = None;
    for key in keys {
        if key.expired {
            continue;
        }
        if microwave_crypto::verify_request(&key.public_key, auth_headers, canonical).is_ok() {
            verified_with = Some(key.key_name);
            break;
        }
    }
    let Some(key_name) = verified_with else {
        return Ok(None);
    };

    let Some(user) = state
        .store
        .find_user(user_id)
        .await
        .map_err(ApiError::from)?
    else {
        return Ok(None);
    };
    let principal = microwave_store::Principal {
        actor_id: user.id,
        authz_id: user.authz_id,
        name: user.username,
        actor_type: ActorType::User,
        org_id: org.map(|org| org.id.clone()),
    };
    Ok(Some((principal, key_name)))
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

fn authentication_failure(user_id: &str) -> ApiError {
    ApiError::unauthorized_with(format!(
        "Failed to authenticate as '{user_id}'. Ensure that your node_name and client key are correct."
    ))
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

/// `/organizations/:org/users/:user/keys` exposes only GET handlers.
fn is_org_user_keys_path(path: &str) -> bool {
    let mut parts = path.trim_matches('/').split('/');
    parts.next() == Some("organizations")
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next() == Some("users")
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next() == Some("keys")
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

/// Upstream's principal endpoint has no authentication (`auth_info` returns
/// `authorized`), so it must work for unassociated users and even for requests
/// whose signature cannot be verified.
fn is_principal_path(path: &str) -> bool {
    let mut parts = path.trim_matches('/').split('/');
    parts.next() == Some("organizations")
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next() == Some("principals")
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next().is_none()
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
    normalize_allow_header(&mut response);
    response
}

/// Axum appends `HEAD` to `GET` routes and joins methods with commas. Erchef
/// lists only the methods declared by the webmachine resource, separated by
/// `", "`, so align the `Allow` header used by 405 responses.
pub(crate) fn normalize_allow_header(response: &mut Response) {
    let Some(value) = response.headers().get(axum::http::header::ALLOW) else {
        return;
    };
    let Ok(value) = value.to_str() else {
        return;
    };
    let methods: Vec<&str> = value
        .split(',')
        .map(str::trim)
        .filter(|method| !method.is_empty() && !method.eq_ignore_ascii_case("HEAD"))
        .collect();
    if let Ok(value) = HeaderValue::from_str(&methods.join(", ")) {
        response
            .headers_mut()
            .insert(axum::http::header::ALLOW, value);
    }
}
