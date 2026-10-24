use crate::error::OAuthError;
use crate::state::OAuthState;
use crate::util::{escape_html, page, parse_params, redirect, require_user, user_json, wants_json};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use microwave_crypto::{generate_rsa_keypair, hash_password, verify_password, PasswordHash};
use microwave_store::ActorType;
use serde_json::json;

pub async fn show(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Response, OAuthError> {
    let Some(user) = crate::util::current_user(&state, &headers)? else {
        return Ok(redirect("/id/signin"));
    };
    if wants_json(&headers) {
        return Ok(Json(user_json(&state, &user)).into_response());
    }

    let body = format!(
        "<h1>Profile</h1>\
         <p>Signed in as: {username}</p>\
         <form method=\"post\" action=\"/id/profile\">\
         <input type=\"hidden\" name=\"_method\" value=\"PUT\">\
         <label for=\"user_username\">Username</label>\
         <input id=\"user_username\" name=\"user[username]\" value=\"{username}\" readonly>\
         <label for=\"user_email\">Email</label>\
         <input id=\"user_email\" name=\"user[email]\" value=\"{email}\">\
         <button type=\"submit\">Save Changes</button>\
         </form>\
         <h2>Change Password</h2>\
         <form method=\"post\" action=\"/id/profile/password\">\
         <input type=\"hidden\" name=\"_method\" value=\"PUT\">\
         <label for=\"current_password\">Current password</label>\
         <input id=\"current_password\" name=\"current_password\" type=\"password\" required>\
         <label for=\"new_password\">New password</label>\
         <input id=\"new_password\" name=\"new_password\" type=\"password\" required>\
         <label for=\"password_confirmation\">Confirm password</label>\
         <input id=\"password_confirmation\" name=\"password_confirmation\" type=\"password\" required>\
         <button type=\"submit\">Change Password</button>\
         </form>\
         <h2>Key</h2>\
         <form method=\"post\" action=\"/id/profile/regen_key\">\
         <button type=\"submit\">Get a New Key</button>\
         </form>",
        username = escape_html(&user.username),
        email = escape_html(user.email.as_deref().unwrap_or(""))
    );
    Ok(page("Profile", &body).into_response())
}

pub async fn update(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, OAuthError> {
    let user = require_user(&state, &headers)?;
    let params = parse_params(request).await?;
    let email = params
        .get_path(&["user", "email"])
        .or_else(|| params.get("email"));
    let updated = state
        .store
        .update_user(&user.username, email.as_deref(), None, None)?;

    if params.is_json() || wants_json(&headers) {
        return Ok(Json(user_json(&state, &updated)).into_response());
    }
    Ok(redirect("/id/profile"))
}

pub async fn change_password(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, OAuthError> {
    let user = require_user(&state, &headers)?;
    let params = parse_params(request).await?;
    let current_password = params.get("current_password").unwrap_or_default();
    let new_password = params.get("new_password").unwrap_or_default();
    let confirmation = params.get("password_confirmation").unwrap_or_default();

    let stored = PasswordHash {
        hash: user.password_hash.clone(),
        salt: user.password_salt.clone(),
        hash_type: user.hash_type.clone(),
    };
    if !verify_password(&current_password, &stored) {
        return Err(OAuthError::forbidden("current password is incorrect"));
    }
    if new_password.is_empty() {
        return Err(OAuthError::invalid_request(
            "new password must not be empty",
        ));
    }
    if new_password != confirmation {
        return Err(OAuthError::invalid_request("passwords do not match"));
    }

    let hash = hash_password(&new_password)
        .map_err(|_| OAuthError::invalid_request("new password must not be empty"))?;
    state.store.update_user(
        &user.username,
        None,
        Some((&hash.hash, &hash.salt, &hash.hash_type)),
        None,
    )?;

    if params.is_json() || wants_json(&headers) {
        return Ok(Json(json!({ "status": "ok" })).into_response());
    }
    Ok(redirect("/id/profile"))
}

pub async fn email(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Response, OAuthError> {
    let user = require_user(&state, &headers)?;
    if wants_json(&headers) {
        return Ok(Json(json!({ "email": user.email })).into_response());
    }
    let body = format!(
        "<h1>Email</h1><p>{}</p><p><a href=\"/id/profile\">Back to profile</a></p>",
        escape_html(user.email.as_deref().unwrap_or(""))
    );
    Ok(page("Email", &body).into_response())
}

pub async fn regen_key(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Response, OAuthError> {
    let user = require_user(&state, &headers)?;
    let (private_key, public_key) = generate_rsa_keypair().map_err(|_| OAuthError::internal())?;

    match state.store.update_actor_key(
        &user.id,
        ActorType::User,
        "default",
        None,
        Some(&public_key),
        None,
    ) {
        Ok(_) => {}
        Err(microwave_store::StoreError::NotFound(_)) => {
            state
                .store
                .add_actor_key(&user.id, ActorType::User, "default", &public_key)?;
        }
        Err(error) => return Err(error.into()),
    }

    let mut response = (StatusCode::OK, Html(private_key)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/pem-keys"),
    );
    let disposition = format!("attachment; filename=\"{}.pem\"", user.username);
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    Ok(response)
}
