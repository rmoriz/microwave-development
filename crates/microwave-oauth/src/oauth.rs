use crate::error::OAuthError;
use crate::state::OAuthState;
use crate::util::{
    append_query, escape_html, page, parse_params, percent_encode, redirect, timestamp_is_fresh,
    wants_json, ACCESS_TOKEN_TTL_SECONDS, AUTHORIZATION_CODE_TTL_SECONDS,
};
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use microwave_store::OAuthApplication;
use serde_json::{json, Value};
use std::collections::HashMap;

struct AuthorizationRequest {
    application: OAuthApplication,
    redirect_uri: String,
    scope: String,
    state: Option<String>,
}

pub async fn authorize(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    uri: Uri,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, OAuthError> {
    let request = validate_authorization(
        &state,
        query.get("client_id").map(String::as_str),
        query.get("redirect_uri").map(String::as_str),
        query.get("response_type").map(String::as_str),
        query.get("scope").map(String::as_str),
        query.get("state").cloned(),
    )
    .await?;

    if crate::util::current_user(&state, &headers).await?.is_none() {
        let return_to = uri
            .path_and_query()
            .map(|value| value.as_str())
            .unwrap_or("/id/oauth/authorize");
        return Ok(redirect(&format!(
            "/id/signin?return_to={}",
            percent_encode(return_to)
        )));
    }

    if wants_json(&headers) {
        return Ok(Json(json!({
            "application": request.application.name,
            "redirect_uri": request.redirect_uri,
            "scope": request.scope
        }))
        .into_response());
    }

    let body = format!(
        "<h1>Authorization Required</h1>\
         <h4>Authorize <strong>{name}</strong> to use your Chef account?</h4>\
         <p>Scopes: {scope}</p>\
         <form method=\"post\" action=\"/id/oauth/authorize\">\
         <input type=\"hidden\" name=\"client_id\" value=\"{uid}\">\
         <input type=\"hidden\" name=\"redirect_uri\" value=\"{redirect_uri}\">\
         <input type=\"hidden\" name=\"response_type\" value=\"code\">\
         <input type=\"hidden\" name=\"state\" value=\"{state}\">\
         <input type=\"hidden\" name=\"scope\" value=\"{scope}\">\
         <button type=\"submit\" name=\"authorize\" value=\"1\">Yes</button>\
         <button type=\"submit\" name=\"deny\" value=\"1\">No, Thanks</button>\
         </form>",
        name = escape_html(&request.application.name),
        uid = escape_html(&request.application.uid),
        redirect_uri = escape_html(&request.redirect_uri),
        state = escape_html(request.state.as_deref().unwrap_or("")),
        scope = escape_html(&request.scope)
    );
    Ok(page("Authorization Required", &body).into_response())
}

pub async fn authorize_approve(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, OAuthError> {
    let params = parse_params(request).await?;
    let request = validate_authorization(
        &state,
        params.get("client_id").as_deref(),
        params.get("redirect_uri").as_deref(),
        params.get("response_type").as_deref(),
        params.get("scope").as_deref(),
        params.get("state"),
    )
    .await?;

    let Some(user) = crate::util::current_user(&state, &headers).await? else {
        if wants_json(&headers) {
            return Err(OAuthError::unauthorized("authentication is required"));
        }
        let location = append_query("/id/signin", &[("return_to", "/id/oauth/authorize")]);
        return Ok(redirect(&location));
    };

    if params.get("deny").is_some() {
        return Ok(redirect(&append_query(
            &request.redirect_uri,
            &[
                ("error", "access_denied"),
                ("error_description", "The resource owner denied the request"),
                ("state", request.state.as_deref().unwrap_or("")),
            ],
        )));
    }

    let grant = state
        .store
        .create_oauth_access_grant(
            &user.id,
            request.application.id,
            &request.redirect_uri,
            &request.scope,
            AUTHORIZATION_CODE_TTL_SECONDS,
        )
        .await?;

    let location = append_query(
        &request.redirect_uri,
        &[
            ("code", grant.token.as_str()),
            ("state", request.state.as_deref().unwrap_or("")),
        ],
    );
    Ok(redirect(&location))
}

pub async fn authorize_deny(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, OAuthError> {
    let params = parse_params(request).await?;
    let request = validate_authorization(
        &state,
        params.get("client_id").as_deref(),
        params.get("redirect_uri").as_deref(),
        params.get("response_type").as_deref(),
        params.get("scope").as_deref(),
        params.get("state"),
    )
    .await?;
    if crate::util::current_user(&state, &headers).await?.is_none() {
        return Ok(redirect("/id/signin"));
    }
    Ok(redirect(&append_query(
        &request.redirect_uri,
        &[
            ("error", "access_denied"),
            ("error_description", "The resource owner denied the request"),
            ("state", request.state.as_deref().unwrap_or("")),
        ],
    )))
}

pub async fn token(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, OAuthError> {
    let params = parse_params(request).await?;
    let (client_id, client_secret) = client_credentials(&headers, &params)?;
    let Some(client_id) = client_id else {
        return Err(OAuthError::invalid_client(
            "client credentials are required",
        ));
    };
    let application = state
        .store
        .find_oauth_application_by_uid(&client_id)
        .await?
        .ok_or_else(|| OAuthError::invalid_client("unknown client"))?;
    if application.confidential && client_secret.as_deref() != Some(application.secret.as_str()) {
        return Err(OAuthError::invalid_client("invalid client secret"));
    }

    let grant_type = params
        .get("grant_type")
        .ok_or_else(|| OAuthError::invalid_request("grant_type is required"))?;

    match grant_type.as_str() {
        "authorization_code" => {
            let code = params
                .get("code")
                .ok_or_else(|| OAuthError::invalid_request("code is required"))?;
            let grant = state
                .store
                .find_oauth_access_grant(&code)
                .await?
                .ok_or_else(|| OAuthError::invalid_grant("authorization code is invalid"))?;
            if grant.revoked_at.is_some()
                || !timestamp_is_fresh(&grant.created_at, grant.expires_in)
            {
                return Err(OAuthError::invalid_grant(
                    "authorization code is expired or already used",
                ));
            }
            if grant.application_id != application.id {
                return Err(OAuthError::invalid_grant(
                    "authorization code belongs to another client",
                ));
            }
            if let Some(redirect_uri) = params.get("redirect_uri") {
                if redirect_uri != grant.redirect_uri {
                    return Err(OAuthError::invalid_grant("redirect_uri does not match"));
                }
            }
            state.store.revoke_oauth_access_grant(&code).await?;
            let token = state
                .store
                .create_oauth_access_token(
                    &grant.resource_owner_id,
                    application.id,
                    &grant.scopes,
                    Some(ACCESS_TOKEN_TTL_SECONDS),
                    true,
                )
                .await?;
            Ok(token_response(&token))
        }
        "refresh_token" => {
            let refresh_token = params
                .get("refresh_token")
                .ok_or_else(|| OAuthError::invalid_request("refresh_token is required"))?;
            let token = state
                .store
                .find_oauth_access_token_by_refresh(&refresh_token)
                .await?
                .ok_or_else(|| OAuthError::invalid_grant("refresh token is invalid"))?;
            if token.revoked_at.is_some() {
                return Err(OAuthError::invalid_grant("refresh token was revoked"));
            }
            if token.application_id != Some(application.id) {
                return Err(OAuthError::invalid_grant(
                    "refresh token belongs to another client",
                ));
            }
            let Some(resource_owner_id) = token.resource_owner_id.clone() else {
                return Err(OAuthError::invalid_grant("refresh token has no owner"));
            };
            state.store.revoke_oauth_access_token(&token.token).await?;
            let token = state
                .store
                .create_oauth_access_token(
                    &resource_owner_id,
                    application.id,
                    &token.scopes,
                    Some(ACCESS_TOKEN_TTL_SECONDS),
                    true,
                )
                .await?;
            Ok(token_response(&token))
        }
        other => Err(OAuthError::new(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            format!("unsupported grant type {other}"),
        )),
    }
}

pub async fn revoke(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, OAuthError> {
    let params = parse_params(request).await?;
    let (client_id, _) = client_credentials(&headers, &params)?;
    let token = params
        .get("token")
        .ok_or_else(|| OAuthError::invalid_request("token is required"))?;

    if let Some(client_id) = client_id {
        let application = state
            .store
            .find_oauth_application_by_uid(&client_id)
            .await?
            .ok_or_else(|| OAuthError::invalid_client("unknown client"))?;
        if let Some(access) = state.store.find_oauth_access_token(&token).await? {
            if access.application_id == Some(application.id) {
                state.store.revoke_oauth_access_token(&token).await?;
            }
        } else if let Some(access) = state
            .store
            .find_oauth_access_token_by_refresh(&token)
            .await?
        {
            if access.application_id == Some(application.id) {
                state
                    .store
                    .revoke_oauth_access_token_by_refresh(&token)
                    .await?;
            }
        } else {
            state.store.revoke_oauth_access_grant(&token).await?;
        }
    } else {
        state.store.revoke_oauth_access_token(&token).await?;
        state
            .store
            .revoke_oauth_access_token_by_refresh(&token)
            .await?;
        state.store.revoke_oauth_access_grant(&token).await?;
    }
    Ok(StatusCode::OK.into_response())
}

async fn validate_authorization(
    state: &OAuthState,
    client_id: Option<&str>,
    redirect_uri: Option<&str>,
    response_type: Option<&str>,
    scope: Option<&str>,
    state_value: Option<String>,
) -> Result<AuthorizationRequest, OAuthError> {
    let client_id = client_id
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OAuthError::invalid_request("client_id is required"))?;
    let application = state
        .store
        .find_oauth_application_by_uid(client_id)
        .await?
        .ok_or_else(|| OAuthError::invalid_client("unknown client"))?;

    let redirect_uri = redirect_uri
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OAuthError::invalid_request("redirect_uri is required"))?;
    let allowed = application
        .redirect_uri
        .split_whitespace()
        .any(|candidate| candidate == redirect_uri);
    if !allowed {
        return Err(OAuthError::invalid_request(
            "redirect_uri is not registered for this client",
        ));
    }

    let response_type = response_type.unwrap_or_default();
    if response_type != "code" {
        return Err(OAuthError::new(
            StatusCode::BAD_REQUEST,
            "unsupported_response_type",
            "only the authorization code flow is supported",
        ));
    }

    let scope = scope
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| (!application.scopes.is_empty()).then(|| application.scopes.clone()))
        .unwrap_or_else(|| "read".to_string());

    Ok(AuthorizationRequest {
        application,
        redirect_uri: redirect_uri.to_string(),
        scope,
        state: state_value,
    })
}

fn client_credentials(
    headers: &HeaderMap,
    params: &crate::util::Params,
) -> Result<(Option<String>, Option<String>), OAuthError> {
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Basic "))
    {
        let decoded = STANDARD
            .decode(value.trim())
            .map_err(|_| OAuthError::invalid_client("invalid basic authorization header"))?;
        let decoded = String::from_utf8(decoded)
            .map_err(|_| OAuthError::invalid_client("invalid basic authorization header"))?;
        let (uid, secret) = decoded
            .split_once(':')
            .ok_or_else(|| OAuthError::invalid_client("invalid basic authorization header"))?;
        return Ok((Some(uid.to_string()), Some(secret.to_string())));
    }
    Ok((params.get("client_id"), params.get("client_secret")))
}

fn token_response(token: &microwave_store::OAuthAccessToken) -> Response {
    let mut body = json!({
        "access_token": token.token,
        "token_type": "bearer",
        "expires_in": token.expires_in,
        "scope": token.scopes,
        "created_at": chrono::DateTime::parse_from_rfc3339(&token.created_at)
            .map(|value| value.timestamp())
            .unwrap_or_default()
    });
    if let Some(refresh_token) = &token.refresh_token {
        body["refresh_token"] = Value::String(refresh_token.clone());
    }
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert(header::PRAGMA, "no-cache".parse().unwrap());
    response
}
