use crate::error::OAuthError;
use crate::state::OAuthState;
use crate::util::{escape_html, page, parse_params, redirect, wants_json};
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use microwave_crypto::hash_password;
use serde_json::json;
use std::collections::HashMap;

pub async fn show(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, OAuthError> {
    let token = query.get("token").filter(|value| !value.is_empty());
    let valid = match token {
        Some(token) => state
            .store
            .find_password_reset_token(token)
            .await?
            .is_some(),
        None => false,
    };

    if wants_json(&headers) {
        return Ok(Json(json!({
            "valid_token": valid,
            "has_token": token.is_some()
        }))
        .into_response());
    }

    let body = if let Some(token) = token.filter(|_| valid) {
        format!(
            "<h1>Reset Password</h1>\
             <form method=\"post\" action=\"/id/password-reset\">\
             <input type=\"hidden\" name=\"_method\" value=\"PUT\">\
             <input type=\"hidden\" name=\"token\" value=\"{token}\">\
             <label for=\"password\">New password</label>\
             <input id=\"password\" name=\"password\" type=\"password\" required>\
             <label for=\"password_confirmation\">Confirm password</label>\
             <input id=\"password_confirmation\" name=\"password_confirmation\" type=\"password\" required>\
             <button type=\"submit\">Reset Password</button></form>",
            token = escape_html(token)
        )
    } else {
        "<h1>Reset Password</h1>\
         <p>Enter your username or email address to request a reset link.</p>\
         <form method=\"post\" action=\"/id/password-reset\">\
         <label for=\"username\">Username or email</label>\
         <input id=\"username\" name=\"username\" required>\
         <button type=\"submit\">Request Reset</button></form>"
            .to_string()
    };
    Ok(page("Reset Password", &body).into_response())
}

pub async fn create(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, OAuthError> {
    let params = parse_params(request).await?;
    if params.get("_method").as_deref() == Some("PUT") {
        return apply_update(&state, &headers, params).await;
    }
    let username = params.get("username").unwrap_or_default();
    if username.is_empty() {
        return Err(OAuthError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            "username is required",
        ));
    }

    let user = find_user(&state, &username).await?;
    let reset_token = match user {
        Some(user) => Some(
            state
                .store
                .create_password_reset_token(&user.username)
                .await?,
        ),
        None => None,
    };
    let reset_url = if state.config.auth.password_reset_token_in_response {
        reset_token
            .as_ref()
            .map(|token| format!("/id/password-reset?token={token}"))
    } else {
        None
    };

    if wants_json(&headers) || params.is_json() {
        let mut payload = json!({
            "status": "ok",
            "notice": "If the account exists, password reset instructions have been generated."
        });
        if let Some(url) = &reset_url {
            payload["reset_url"] = json!(url);
        }
        return Ok(Json(payload).into_response());
    }

    let link = reset_url
        .as_deref()
        .map(|url| {
            format!(
                "<p><a href=\"{}\">Continue to reset your password</a></p>",
                escape_html(url)
            )
        })
        .unwrap_or_default();
    let body = format!(
        "<h1>Reset Password</h1>\
         <p>If the account exists, password reset instructions have been generated.</p>\
         {link}<p><a href=\"/id/signin\">Back to sign in</a></p>"
    );
    Ok(page("Reset Password", &body).into_response())
}

pub async fn update(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, OAuthError> {
    let params = parse_params(request).await?;
    apply_update(&state, &headers, params).await
}

async fn apply_update(
    state: &OAuthState,
    headers: &HeaderMap,
    params: crate::util::Params,
) -> Result<Response, OAuthError> {
    let token = params
        .get("token")
        .ok_or_else(|| OAuthError::invalid_request("token is required"))?;
    let Some(username) = state.store.consume_password_reset_token(&token).await? else {
        return Err(OAuthError::forbidden("reset token is invalid or expired"));
    };
    let password = params.get("password").unwrap_or_default();
    let confirmation = params.get("password_confirmation").unwrap_or_default();
    if password.is_empty() {
        return Err(OAuthError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            "password must not be empty",
        ));
    }
    if password != confirmation {
        return Err(OAuthError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            "passwords do not match",
        ));
    }

    let hash = hash_password(&password)
        .map_err(|_| OAuthError::invalid_request("password must not be empty"))?;
    state
        .store
        .update_user(
            &username,
            None,
            Some((&hash.hash, &hash.salt, &hash.hash_type)),
            None,
        )
        .await?;

    if wants_json(headers) || params.is_json() {
        return Ok(Json(json!({ "status": "ok" })).into_response());
    }
    Ok(redirect("/id/signin"))
}

async fn find_user(
    state: &OAuthState,
    username: &str,
) -> Result<Option<microwave_store::User>, OAuthError> {
    if let Some(user) = state.store.find_user(username).await? {
        return Ok(Some(user));
    }
    if username.contains('@') {
        return Ok(state
            .store
            .list_users()
            .await?
            .into_iter()
            .find(|user| user.email.as_deref() == Some(username)));
    }
    Ok(None)
}
