use crate::error::OAuthError;
use crate::state::OAuthState;
use crate::util::{escape_html, page, parse_params, redirect, wants_json};
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use microwave_crypto::hash_password;
use microwave_mail::MailMessage;
use serde_json::json;
use std::collections::HashMap;

const PASSWORD_RESET_SUBJECT: &str = "Reset your Chef Infra Server password";

pub async fn show(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, OAuthError> {
    let token = query.get("token").filter(|value| !value.is_empty());
    let ttl_seconds = state.config.auth.password_reset_ttl_seconds as i64;
    let valid = match token {
        Some(token) => state
            .store
            .find_password_reset_token(token, ttl_seconds)
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
        Some(ref user) => Some(
            state
                .store
                .create_password_reset_token(&user.username)
                .await?,
        ),
        None => None,
    };

    // Send email if conditions are met
    if let (Some(token), Some(user)) = (reset_token.as_ref(), user.as_ref()) {
        if state.mailer.is_enabled() {
            if let Some(user_email) = &user.email {
                if !user_email.is_empty() {
                    if let Some(from_email) = &state.config.mail.from {
                        let reset_url = absolute_reset_url(&state, token)
                            .unwrap_or_else(|| format!("/id/password-reset?token={token}"));

                        let message = MailMessage {
                            from: from_email.clone(),
                            to: user_email.clone(),
                            reply_to: state.config.mail.reply_to.clone(),
                            subject: PASSWORD_RESET_SUBJECT.to_string(),
                            text_body: password_reset_body(
                                &reset_url,
                                state.config.auth.password_reset_ttl_seconds,
                            ),
                        };

                        if let Err(error) = state.mailer.send(&message).await {
                            tracing::warn!(%error, "password reset mail failed");
                        }
                    }
                }
            }
        }
    }

    let reset_url = if state.config.auth.password_reset_token_in_response {
        reset_token.as_ref().map(|token| {
            absolute_reset_url(&state, token)
                .unwrap_or_else(|| format!("/id/password-reset?token={token}"))
        })
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
    let ttl_seconds = state.config.auth.password_reset_ttl_seconds as i64;
    let Some(username) = state
        .store
        .consume_password_reset_token(&token, ttl_seconds)
        .await?
    else {
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

fn absolute_reset_url(state: &OAuthState, token: &str) -> Option<String> {
    state
        .config
        .server
        .public_url
        .as_ref()
        .filter(|url| !url.is_empty())
        .map(|url| {
            let base = url.trim_end_matches('/');
            format!("{base}/id/password-reset?token={token}")
        })
}

fn password_reset_body(reset_url: &str, ttl_seconds: u64) -> String {
    let minutes = ttl_seconds.div_ceil(60);
    format!(
        "A password reset has been requested for your Chef Infra Server account.\n\n\
         To reset your password, visit: {reset_url}\n\n\
         This link will expire in {minutes} minutes.\n\n\
         If you did not request this reset, you can safely ignore this email."
    )
}
