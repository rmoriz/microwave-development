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
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    // `Router::layer` runs after axum has selected a route, so rewriting the
    // URI here cannot change which handler was chosen. When a rewrite is
    // needed, re-enter the router once with the new path. The marker also
    // prevents recursion and keeps request metrics counted once.
    if let Some(original) = request.extensions().get::<OriginalRequestPath>().cloned() {
        // A prior pass already removed a trailing slash. The default-org
        // rewrite may still be needed, so do not skip routing entirely.
        let routed_path = routing_path(&original.0, state.config.server.default_orgname.as_deref());
        if routed_path != request.uri().path() {
            if let Some(uri) = rewrite_uri_path(request.uri(), &routed_path) {
                *request.uri_mut() = uri;
            }
            let response =
                match tower::ServiceExt::oneshot(crate::router(state.clone()), request).await {
                    Ok(response) => response,
                    Err(error) => match error {},
                };
            return response;
        }
        return authenticate_request(state, request, next).await;
    }

    state.metrics.record_request();

    // Clients sign the URI they sent, so keep that path and only rewrite the
    // request the router dispatches.
    let signed_path = request.uri().path().to_string();
    let routed_path = routing_path(&signed_path, state.config.server.default_orgname.as_deref());
    if routed_path != signed_path {
        if let Some(uri) = rewrite_uri_path(request.uri(), &routed_path) {
            *request.uri_mut() = uri;
        }
        request
            .extensions_mut()
            .insert(OriginalRequestPath(signed_path));

        let response = match tower::ServiceExt::oneshot(crate::router(state.clone()), request).await
        {
            Ok(response) => response,
            Err(error) => match error {},
        };
        state.metrics.record_response(response.status());
        return response;
    }

    let response = authenticate_request(state.clone(), request, next).await;
    state.metrics.record_response(response.status());
    response
}

/// The path the client signed, before [`routing_path`] rewrote it.
#[derive(Clone, Debug)]
pub(crate) struct OriginalRequestPath(pub(crate) String);

/// Top-level compatibility endpoints upstream nginx maps to `default_orgname`.
const DEFAULT_ORG_ENDPOINTS: [&str; 11] = [
    "search",
    "nodes",
    "cookbooks",
    "data",
    "roles",
    "sandboxes",
    "environments",
    "clients",
    "principals",
    "groups",
    "containers",
];

/// Compute the path used for routing.
///
/// A single trailing slash is removed (matching upstream's `p_maybe_sep`), and
/// top-level compatibility endpoints are prefixed with the configured default
/// organization. The original path remains available for signature
/// verification.
fn routing_path(path: &str, default_orgname: Option<&str>) -> String {
    let path = without_one_trailing_slash(path);
    match default_orgname {
        Some(orgname) if is_default_org_endpoint(path) => {
            format!("/organizations/{orgname}{path}")
        }
        _ => path.to_string(),
    }
}

/// Strip exactly one trailing slash, leaving `/` and double slashes alone.
fn without_one_trailing_slash(path: &str) -> &str {
    if path.len() > 1 && path.ends_with('/') && !path.ends_with("//") {
        &path[..path.len() - 1]
    } else {
        path
    }
}

/// Normalize one trailing slash before axum selects a route.
///
/// This wrapper runs before route matching, so path parameters are extracted
/// from the normalized URI; doing it after matching leaves stale captures for
/// wildcard routes such as compliance profiles.
pub(crate) fn normalize_trailing_slash(request: &mut axum::extract::Request) {
    let signed_path = request.uri().path().to_string();
    let routed_path = without_one_trailing_slash(&signed_path);
    if routed_path != signed_path {
        if let Some(uri) = rewrite_uri_path(request.uri(), routed_path) {
            *request.uri_mut() = uri;
        }
        request
            .extensions_mut()
            .insert(OriginalRequestPath(signed_path));
    }
}

/// True for `/nodes`, `/nodes/...`, `/search`, and the other upstream
/// default-org compatibility endpoints.
fn is_default_org_endpoint(path: &str) -> bool {
    path.strip_prefix('/')
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|endpoint| DEFAULT_ORG_ENDPOINTS.contains(&endpoint))
}

/// Replace only the path portion of a URI, preserving its query and any
/// scheme/authority from HTTP/2 request targets.
fn rewrite_uri_path(uri: &axum::http::Uri, path: &str) -> Option<axum::http::Uri> {
    let path_and_query = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_string(),
    };
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(path_and_query.parse().ok()?);
    axum::http::Uri::from_parts(parts).ok()
}

/// Give handlers a `Host` header for HTTP/2 requests.
///
/// HTTP/2 clients send `:authority` instead of a `Host` header. Hyper exposes
/// it as the request URI authority, while code that builds absolute URLs reads
/// `Host`/`X-Forwarded-Host`. Inserting the authority keeps URL generation
/// correct without changing HTTP/1.1 or proxy precedence.
pub async fn ensure_host_header(
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    use axum::http::header::HOST;

    if !request.headers().contains_key(HOST) && !request.headers().contains_key("x-forwarded-host")
    {
        if let Some(authority) = request.uri().authority() {
            if let Ok(value) = HeaderValue::from_str(authority.as_str()) {
                request.headers_mut().insert(HOST, value);
            }
        }
    }
    next.run(request).await
}

async fn authenticate_request(
    state: crate::AppState,
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

    // Maintenance mode rejects normal API traffic before authentication runs,
    // so an unauthenticated request during a drain cannot mutate the store.
    // Health checks stay reachable.
    if !is_maintenance_exempt(&path) {
        match state.store.maintenance_enabled().await {
            Ok(true) => {
                return with_version_header(
                    ApiError::service_unavailable("Server is in maintenance mode.").into_response(),
                    &requested_version.to_string(),
                    &requested_version.to_string(),
                );
            }
            Ok(false) => {}
            Err(error) => {
                tracing::error!(error = %error, "failed to read maintenance state");
                return with_version_header(
                    ApiError::from(error).into_response(),
                    &requested_version.to_string(),
                    &requested_version.to_string(),
                );
            }
        }
    }

    // Internal compatibility resources authenticate before their handlers run,
    // but webmachine still answers an unsupported method with 405 first. Let
    // those requests through to the router so the `Allow` header is generated
    // there without requiring a valid signature.
    if let Some(allowed) = internal_allowed_methods(&path) {
        if !allowed.contains(&request.method().as_str()) {
            let response = next.run(request).await;
            return with_version_header(
                response,
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

    // Optional compatibility surfaces are defined by nginx upstream rather
    // than by the signed API. Match their pre-auth semantics: a disabled
    // required recipe is always 404 for every method, a non-GET is 405 before
    // authentication, and data collector requests may authenticate with the
    // configured shared token instead of a Chef signature.
    if is_required_recipe_path(&path) {
        let enabled = state.config.required_recipe.enabled;
        let method = request.method();
        // Axum's `get()` also serves HEAD. Treat HEAD like GET for the auth
        // gate so it cannot bypass signature verification.
        let is_read = method == axum::http::Method::GET || method == axum::http::Method::HEAD;
        if !enabled || !is_read {
            let response = next.run(request).await;
            return with_version_header(
                response,
                &requested_version.to_string(),
                &requested_version.to_string(),
            );
        }
        if !has_signing_headers(request.headers()) {
            let response = ApiError::bad_request("Invalid request").into_response();
            return with_version_header(
                response,
                &requested_version.to_string(),
                &requested_version.to_string(),
            );
        }
    }

    if is_controls_path(&path) {
        // Upstream `oc_chef_wm_controls` halts in `service_available/2`, so it
        // answers 410 before authentication or organization resolution.
        let response = next.run(request).await;
        return with_version_header(
            response,
            &requested_version.to_string(),
            &requested_version.to_string(),
        );
    }

    if is_data_collector_path(&path) {
        let configured = state.config.data_collector.is_configured();
        // Upstream rejects non-POST before authentication. Do the same while
        // the route is enabled, even when the caller forgot the token.
        if configured && request.method() != axum::http::Method::POST {
            let response = next.run(request).await;
            return with_version_header(
                response,
                &requested_version.to_string(),
                &requested_version.to_string(),
            );
        }
        if !configured || crate::data_collector::has_valid_token(&state, request.headers()).await {
            let response = next.run(request).await;
            return with_version_header(
                response,
                &requested_version.to_string(),
                &requested_version.to_string(),
            );
        }
    }

    if is_compliance_proxy_path(&path) {
        let enabled = state.config.compliance_proxy.is_configured();
        let method = request.method();
        // The proxy is GET-only upstream. A disabled proxy is absent from the
        // router, so bypass authentication and let the fallback return 404 for
        // every method and auth state.
        let is_read = method == axum::http::Method::GET || method == axum::http::Method::HEAD;
        if !enabled || !is_read {
            let response = next.run(request).await;
            return with_version_header(
                response,
                &requested_version.to_string(),
                &requested_version.to_string(),
            );
        }
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
        || path == "/_stats"
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
    let signed_path = parts
        .extensions
        .get::<OriginalRequestPath>()
        .map(|original| original.0.clone())
        .unwrap_or_else(|| path.clone());
    let method = parts.method.as_str().to_string();
    // `chef_wm_validate` signs the path below `/validate/`, not the validation
    // URL itself. The body is still the body that was actually sent:
    // `chef_authn` hashes it and rejects a mismatched `X-Ops-Content-Hash`.
    //
    // Routing may have rewritten `path` (default-org prefix or a trailing
    // slash), but the canonical request always uses the URI the client signed.
    let validated_path = validate_signed_path(&signed_path);
    let canonical_path = validated_path.as_deref().unwrap_or(signed_path.as_str());
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
        path: canonical_path,
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
            && !(principal.actor_type == ActorType::User
                && principal.name == state.config.auth.superuser_name)
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
    let mut action_meta = if state.actions.is_enabled()
        && crate::actions::is_candidate(parts.method.as_str(), &path)
    {
        crate::actions::RequestMeta::capture(
            &parts.method,
            &path,
            &parts.headers,
            body_bytes.clone(),
            &principal,
            org.as_ref(),
        )
    } else {
        None
    };
    if let Some(meta) = action_meta.as_mut() {
        if let Some(username) =
            crate::actions::resolve_invitation_username(state.store.as_ref(), &path).await
        {
            meta.set_resolved_username(username);
        }
    }
    let mut request = axum::extract::Request::from_parts(parts, axum::body::Body::from(body_bytes));
    request
        .extensions_mut()
        .insert(crate::state::ApiVersion(api_version));
    request.extensions_mut().insert(crate::state::AuthContext {
        is_superuser: principal.actor_type == ActorType::User
            && principal.name == state.config.auth.superuser_name,
        principal,
        org,
        key_name: Some(key_name),
    });

    let response = next.run(request).await;
    if let Some(meta) = action_meta {
        state.actions.notify(meta, response.status()).await;
    }
    Ok(response)
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
        .find_user(&state.config.auth.superuser_name)
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

/// Liveness and metrics endpoints remain available while the API is drained
/// for maintenance.
fn is_maintenance_exempt(path: &str) -> bool {
    path == "/_status" || path == "/_stats"
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

/// `/organizations/:org/data-collector` (exact, no trailing slash).
fn is_data_collector_path(path: &str) -> bool {
    let mut parts = path.trim_matches('/').split('/');
    parts.next() == Some("organizations")
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next() == Some("data-collector")
        && parts.next().is_none()
}

/// `/organizations/:org/controls` (exact, no trailing slash).
fn is_controls_path(path: &str) -> bool {
    let mut parts = path.trim_matches('/').split('/');
    parts.next() == Some("organizations")
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next() == Some("controls")
        && parts.next().is_none()
}

/// `/organizations/:org/validate/<signed path>`.
///
/// Returns the path the request signature covers, or `None` when the request
/// is not a validation request. `chef_wm_validate` turns the wildcard match
/// back into an absolute path before handing it to signature verification.
fn validate_signed_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/organizations/")?;
    let (_org, rest) = rest.split_once('/')?;
    let rest = rest.strip_prefix("validate/")?;
    if rest.is_empty() {
        return None;
    }
    Some(format!("/{rest}"))
}

/// Methods accepted by internal resources that must answer 405 before
/// authentication.
///
/// Upstream resolves method dispatch before `is_authorized/2`, so an
/// unsupported method never verifies the signature. The gate only covers
/// routes that are actually unauthenticated-internal or read-only; ordinary
/// mutating routes that happen to share a path shape still authenticate
/// normally.
///
/// `HEAD` accompanies GET because axum routes it to the GET handler. The
/// router strips it from the generated `Allow` header.
fn internal_allowed_methods(path: &str) -> Option<&'static [&'static str]> {
    if validate_signed_path(path).is_some() {
        return Some(&["GET", "HEAD", "POST", "PUT", "DELETE"]);
    }
    if is_node_identifiers_path(path) || is_role_environment_path(path) {
        return Some(&["GET", "HEAD"]);
    }
    None
}

/// `/organizations/:org/nodes/:name/_identifiers`.
///
/// Only nodes are an upstream internal route. A broader `_identifiers` match
/// would bypass authentication for real mutating routes such as
/// `/data/:bag/_identifiers` or `/cookbooks/:cookbook/_identifiers`.
fn is_node_identifiers_path(path: &str) -> bool {
    let mut parts = path.trim_matches('/').split('/');
    parts.next() == Some("organizations")
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next() == Some("nodes")
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next() == Some("_identifiers")
        && parts.next().is_none()
}

/// The read-only role/environment run-list resources:
///
/// * `/organizations/:org/roles/:role/environments`
/// * `/organizations/:org/roles/:role/environments/:environment`
/// * `/organizations/:org/environments/:environment/roles/:role`
fn is_role_environment_path(path: &str) -> bool {
    let mut parts = path.trim_matches('/').split('/');
    if parts.next() != Some("organizations") {
        return false;
    }
    if !parts.next().is_some_and(|part| !part.is_empty()) {
        return false;
    }
    let (Some(third), Some(fourth), Some(fifth)) = (parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let sixth = parts.next();
    if parts.next().is_some() || fourth.is_empty() {
        return false;
    }
    if third == "roles" && fifth == "environments" {
        return sixth.is_none_or(|part| !part.is_empty());
    }
    third == "environments" && fifth == "roles" && sixth.is_some_and(|part| !part.is_empty())
}

/// `/organizations/:org/owners/:owner/compliance[/...]` and the legacy
/// `/compliance/organizations/:org/owners/:owner/compliance[/...]` spelling.
fn is_compliance_proxy_path(path: &str) -> bool {
    let parts: Vec<&str> = path
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let parts = if parts.first() == Some(&"compliance") {
        &parts[1..]
    } else {
        &parts[..]
    };
    parts.len() >= 5
        && parts.first() == Some(&"organizations")
        && parts.get(1).is_some_and(|part| !part.is_empty())
        && parts.get(2) == Some(&"owners")
        && parts.get(3).is_some_and(|part| !part.is_empty())
        && parts.get(4) == Some(&"compliance")
}

/// `/organizations/:org/required_recipe` (exact, no trailing slash).
fn is_required_recipe_path(path: &str) -> bool {
    let mut parts = path.trim_matches('/').split('/');
    parts.next() == Some("organizations")
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next() == Some("required_recipe")
        && parts.next().is_none()
}

fn has_signing_headers(headers: &HeaderMap) -> bool {
    [
        "x-ops-sign",
        "x-ops-userid",
        "x-ops-timestamp",
        "x-ops-content-hash",
    ]
    .iter()
    .all(|name| headers.contains_key(*name))
        && headers
            .keys()
            .any(|name| name.as_str().starts_with("x-ops-authorization-"))
}

fn organization_name_from_path(path: &str) -> Option<String> {
    let mut parts = path.trim_matches('/').split('/');
    let first = parts.next()?;
    if first == "compliance" {
        // The legacy chef-gate spelling is
        // `/compliance/organizations/:org/...`.
        if parts.next() != Some("organizations") {
            return None;
        }
    } else if first != "organizations" {
        return None;
    }
    parts
        .next()
        .filter(|part| !part.is_empty())
        .map(ToString::to_string)
}

/// Upstream's principal endpoint has no authentication (`auth_info` returns
/// `authorized`), so it must work for unassociated users and even for requests
/// whose signature cannot be verified.
fn is_principal_path(path: &str) -> bool {
    let mut parts = path.trim_matches('/').split('/');
    parts.next() == Some("organizations")
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next() == Some("principals")
        && match parts.next() {
            None => true,
            Some(name) => !name.is_empty() && parts.next().is_none(),
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

#[cfg(test)]
mod tests {
    use super::{is_compliance_proxy_path, organization_name_from_path};

    #[test]
    fn extracts_the_organization_from_standard_and_legacy_compliance_paths() {
        assert_eq!(
            organization_name_from_path("/organizations/acme/nodes").as_deref(),
            Some("acme")
        );
        assert_eq!(
            organization_name_from_path("/compliance/organizations/acme/owners/foo/compliance")
                .as_deref(),
            Some("acme")
        );
        assert_eq!(organization_name_from_path("/users/alice"), None);
        assert_eq!(organization_name_from_path("/compliance/owners/acme"), None);
    }

    #[test]
    fn recognizes_both_compliance_proxy_spellings() {
        assert!(is_compliance_proxy_path(
            "/organizations/acme/owners/foo/compliance"
        ));
        assert!(is_compliance_proxy_path(
            "/compliance/organizations/acme/owners/foo/compliance/profile"
        ));
        assert!(!is_compliance_proxy_path(
            "/organizations/acme/owners/foo/nodes"
        ));
    }
}
