use crate::error::OAuthError;
use crate::state::OAuthState;
use crate::util::{
    cookie_header, csrf_token, current_user, delete_cookie_header, escape_html, page,
    page_with_token, parse_params, redirect, session_token, wants_json,
};
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use microwave_crypto::{verify_password, PasswordHash};
use microwave_store::User;
use serde_json::json;
use std::collections::HashMap;

pub async fn home(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Response, OAuthError> {
    if wants_json(&headers) {
        return Ok(Json(json!({
            "status": "ok",
            "service": "oc-id",
            "authenticated": current_user(&state, &headers).await?.is_some()
        }))
        .into_response());
    }
    Ok(page(
        "Chef Identity",
        "<h1>Chef Identity</h1>\
         <p><a href=\"/id/signin\">Sign in</a></p>\
         <p><a href=\"/id/oauth/applications\">Applications</a></p>\
         <p><a href=\"/id/v1/status\">Status</a></p>",
    )
    .into_response())
}

pub async fn signin(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, OAuthError> {
    if wants_json(&headers) {
        return Ok(Json(json!({
            "authenticated": current_user(&state, &headers).await?.is_some()
        }))
        .into_response());
    }

    // Upstream `SessionsController#new` redirects an already signed-in user to
    // the authorized-applications page.
    if current_user(&state, &headers).await?.is_some() {
        return Ok(redirect("/id/oauth/authorized_applications"));
    }

    let return_to = query
        .get("return_to")
        .map(|value| {
            format!(
                "<input type=\"hidden\" name=\"return_to\" value=\"{}\">",
                escape_html(value)
            )
        })
        .unwrap_or_default();
    let csrf = csrf_token();
    let body = format!(
        "<h1>Sign In</h1>\
         <p>Please sign in with your Chef account.</p>\
         <form method=\"post\" action=\"/id/auth/chef/callback\">\
         <input type=\"hidden\" name=\"authenticity_token\" value=\"{csrf}\">\
         {return_to}\
         <label for=\"username\">Username or email</label>\
         <input id=\"username\" name=\"username\" autofocus required>\
         <label for=\"password\">Password</label>\
         <input id=\"password\" name=\"password\" type=\"password\" required>\
         <button type=\"submit\" name=\"commit\" value=\"Sign In\">Sign In</button>\
         </form>\
         <p><a href=\"/id/password-reset\">Forgot your password?</a></p>"
    );
    let mut response = page_with_token("Sign In", &body, &csrf).into_response();
    if session_token(&headers).is_none() {
        let pre_session = uuid::Uuid::new_v4().simple().to_string();
        set_cookie(&mut response, &cookie_header(&pre_session))?;
    }
    Ok(response)
}

pub async fn create_session(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Result<Response, OAuthError> {
    let params = parse_params(request).await?;
    let username = params
        .get("username")
        .or_else(|| query.get("username").cloned())
        .unwrap_or_default();
    let password = params
        .get("password")
        .or_else(|| query.get("password").cloned())
        .unwrap_or_default();

    let mut user = state.store.find_user(&username).await?;
    if user.is_none() && username.contains('@') {
        user = state.store.list_users().await.ok().and_then(|users| {
            users
                .into_iter()
                .find(|candidate| candidate.email.as_deref() == Some(username.as_str()))
        });
    }

    let authenticated = user.filter(|user| {
        verify_password(
            &password,
            &PasswordHash {
                hash: user.password_hash.clone(),
                salt: user.password_salt.clone(),
                hash_type: user.hash_type.clone(),
            },
        )
    });

    let Some(user) = authenticated else {
        if wants_json(&headers) {
            return Err(OAuthError::unauthorized("invalid credentials"));
        }
        return Ok(redirect(
            "/id/auth/failure?message=invalid_credentials&strategy=chef",
        ));
    };

    let token = state.store.create_id_session(&user.username).await?;
    let cookie = cookie_header(&token);

    if wants_json(&headers) {
        let mut response = Json(json!({
            "username": user.username,
            "session_token": token
        }))
        .into_response();
        set_cookie(&mut response, &cookie)?;
        return Ok(response);
    }

    let location = params
        .get("return_to")
        .or_else(|| query.get("return_to").cloned())
        .filter(|value| value.starts_with('/') && !value.starts_with("//"))
        .unwrap_or_else(|| "/id/oauth/authorized_applications".to_string());
    let mut response = redirect(&location);
    set_cookie(&mut response, &cookie)?;
    Ok(response)
}

pub async fn signout(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Response, OAuthError> {
    if let Some(token) = session_token(&headers) {
        state.store.delete_id_session(&token).await?;
    }
    if wants_json(&headers) {
        let mut response = Json(json!({})).into_response();
        set_cookie(&mut response, &delete_cookie_header())?;
        return Ok(response);
    }
    let mut response = redirect("/id/signin");
    set_cookie(&mut response, &delete_cookie_header())?;
    Ok(response)
}

pub async fn auth_failure(headers: HeaderMap) -> Response {
    if wants_json(&headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "invalid_credentials" })),
        )
            .into_response();
    }
    (
        StatusCode::UNAUTHORIZED,
        Html(
            "<h1>Sign in failed</h1><p>Invalid username or password.</p>\
             <p><a href=\"/id/signin\">Try again</a></p>"
                .to_string(),
        ),
    )
        .into_response()
}

pub async fn authorized_applications(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Response, OAuthError> {
    let user: User = match current_user(&state, &headers).await? {
        Some(user) => user,
        None => return Ok(redirect("/id/signin")),
    };
    if wants_json(&headers) {
        return Ok(Json(json!({ "applications": [] })).into_response());
    }
    let body = format!(
        "<h1>Authorized Applications</h1>\
         <p>Signed in as: {}</p>\
         <p><a href=\"/id/profile\">Profile</a></p>\
         <form method=\"post\" action=\"/id/signout\">\
         <button type=\"submit\">Sign Out</button></form>",
        escape_html(&user.username)
    );
    Ok(page("Authorized Applications", &body).into_response())
}

fn set_cookie(response: &mut Response, cookie: &str) -> Result<(), OAuthError> {
    let value = HeaderValue::from_str(cookie).map_err(|_| OAuthError::internal())?;
    response.headers_mut().insert(header::SET_COOKIE, value);
    Ok(())
}
