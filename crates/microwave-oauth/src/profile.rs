use crate::error::OAuthError;
use crate::state::OAuthState;
use crate::util::{
    csrf_token, escape_html, page, page_with_token, parse_params, redirect, require_user,
    user_json, wants_json,
};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use microwave_crypto::{generate_rsa_keypair, hash_password, verify_password, PasswordHash};
use microwave_store::{ActorType, UserPatch};
use serde_json::json;

pub async fn show(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Response, OAuthError> {
    let Some(user) = crate::util::current_user(&state, &headers).await? else {
        return Ok(redirect("/id/signin"));
    };
    if wants_json(&headers) {
        return Ok(Json(user_json(&state, &user).await).into_response());
    }

    let csrf = csrf_token();
    let body = format!(
        "<h1>Profile</h1>\
         <p>Signed in as: {username}</p>\
         <form method=\"post\" action=\"/id/profile\">\
         <input type=\"hidden\" name=\"_method\" value=\"PUT\">\
         <input type=\"hidden\" name=\"authenticity_token\" value=\"{csrf}\">\
         <label for=\"user_first_name\">First name</label>\
         <input type=\"text\" name=\"user[first_name]\" value=\"{first_name}\" id=\"user_first_name\" />\
         <label for=\"user_middle_name\">Middle name</label>\
         <input type=\"text\" name=\"user[middle_name]\" value=\"{middle_name}\" id=\"user_middle_name\" />\
         <label for=\"user_last_name\">Last name</label>\
         <input type=\"text\" name=\"user[last_name]\" value=\"{last_name}\" id=\"user_last_name\" />\
         <label for=\"user_email\">Email</label>\
         <input type=\"text\" name=\"user[email]\" value=\"{email}\" id=\"user_email\" />\
         <button type=\"submit\" name=\"commit\" value=\"Save Changes\">Save Changes</button>\
         </form>\
         <h2>Change Password</h2>\
         <form method=\"post\" action=\"/id/profile/password\">\
         <input type=\"hidden\" name=\"_method\" value=\"PUT\">\
         <input type=\"hidden\" name=\"authenticity_token\" value=\"{csrf}\">\
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
         <input type=\"hidden\" name=\"authenticity_token\" value=\"{csrf}\">\
         <button type=\"submit\" name=\"commit\" value=\"Get a New Key\">Get a New Key</button>\
         </form>",
        username = escape_html(&user.username),
        first_name = escape_html(user.first_name.as_deref().unwrap_or("")),
        middle_name = escape_html(user.middle_name.as_deref().unwrap_or("")),
        last_name = escape_html(user.last_name.as_deref().unwrap_or("")),
        email = escape_html(user.email.as_deref().unwrap_or(""))
    );
    Ok(page_with_token("Profile", &body, &csrf).into_response())
}

pub async fn update(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, OAuthError> {
    let params = parse_params(request).await?;
    let Some(user) = crate::util::current_user(&state, &headers).await? else {
        if params.is_json() || wants_json(&headers) {
            return Err(OAuthError::unauthorized("authentication is required"));
        }
        return Ok(redirect("/id/signin"));
    };

    // Upstream's profile form only exposes the name fields. The email field is
    // present but an address change goes through a verification link, so the
    // stored address is deliberately left untouched here.
    let first_name = params
        .get_path(&["user", "first_name"])
        .or_else(|| params.get("first_name"))
        .map(Some);
    let middle_name = params
        .get_path(&["user", "middle_name"])
        .or_else(|| params.get("middle_name"))
        .map(Some);
    let last_name = params
        .get_path(&["user", "last_name"])
        .or_else(|| params.get("last_name"))
        .map(Some);
    let display_name = params
        .get_path(&["user", "display_name"])
        .or_else(|| params.get("display_name"))
        .map(Some);

    let patch = UserPatch {
        first_name,
        middle_name,
        last_name,
        display_name,
        ..Default::default()
    };
    let updated = state
        .store
        .update_user_patch(&user.username, &patch)
        .await?;

    // Upstream oc-id round-trips the current public key through `PUT /users`
    // when the profile form is saved. That path leaves the key with an extra
    // trailing newline, which the Pedant oc_id spec asserts on the following
    // `GET /users/:name` (`public_key.to_s + "\n"`). Mirror the whitespace
    // without changing the key material.
    normalize_default_key(&state, &updated).await?;

    if params.is_json() || wants_json(&headers) {
        return Ok(Json(user_json(&state, &updated).await).into_response());
    }
    Ok(redirect("/id/profile"))
}

async fn normalize_default_key(
    state: &OAuthState,
    user: &microwave_store::User,
) -> Result<(), OAuthError> {
    let Some(key) = state
        .store
        .find_actor_key(&user.id, ActorType::User, "default")
        .await?
    else {
        return Ok(());
    };
    let normalized = format!("{}\n\n", key.public_key.trim_end_matches('\n'));
    if normalized != key.public_key {
        state
            .store
            .update_actor_key(
                &user.id,
                ActorType::User,
                "default",
                None,
                Some(&normalized),
                None,
            )
            .await?;
    }
    Ok(())
}

pub async fn change_password(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, OAuthError> {
    let user = require_user(&state, &headers).await?;
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
    state
        .store
        .update_user(
            &user.username,
            None,
            Some((&hash.hash, &hash.salt, &hash.hash_type)),
            None,
        )
        .await?;

    if params.is_json() || wants_json(&headers) {
        return Ok(Json(json!({ "status": "ok" })).into_response());
    }
    Ok(redirect("/id/profile"))
}

pub async fn email(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Response, OAuthError> {
    let user = require_user(&state, &headers).await?;
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
    let user = require_user(&state, &headers).await?;
    let (private_key, public_key) = generate_rsa_keypair().map_err(|_| OAuthError::internal())?;

    match state
        .store
        .update_actor_key(
            &user.id,
            ActorType::User,
            "default",
            None,
            Some(&public_key),
            None,
        )
        .await
    {
        Ok(_) => {}
        Err(microwave_store::StoreError::NotFound(_)) => {
            state
                .store
                .add_actor_key(&user.id, ActorType::User, "default", &public_key)
                .await?;
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
