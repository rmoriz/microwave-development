use crate::error::OAuthError;
use crate::state::OAuthState;
use crate::util::{
    csrf_token, escape_html, page, page_with_token, parse_params, redirect, require_admin,
    wants_json,
};
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use microwave_store::OAuthApplication;
use serde_json::{json, Value};

pub async fn new_application(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Response, OAuthError> {
    require_admin(&state, &headers).await?;
    if wants_json(&headers) {
        return Ok(Json(json!({
            "name": Value::Null,
            "redirect_uri": Value::Null,
            "scopes": "",
            "confidential": true
        }))
        .into_response());
    }
    let csrf = csrf_token();
    let body = format!(
        "<h1>New application</h1>\
         <form method=\"post\" action=\"/id/oauth/applications\">\
         <input type=\"hidden\" name=\"authenticity_token\" value=\"{csrf}\">\
         <label for=\"name\">Name</label><input id=\"name\" name=\"name\" required>\
         <label for=\"redirect_uri\">Callback URL</label>\
         <textarea id=\"redirect_uri\" name=\"redirect_uri\" required></textarea>\
         <button type=\"submit\">Submit</button></form>\
         <p><a href=\"/id/oauth/applications\">Cancel</a></p>"
    );
    Ok(page_with_token("New application", &body, &csrf).into_response())
}

pub async fn list_applications(
    State(state): State<OAuthState>,
    headers: HeaderMap,
) -> Result<Response, OAuthError> {
    require_admin(&state, &headers).await?;
    let applications = state.store.list_oauth_applications().await?;
    if wants_json(&headers) {
        let list: Vec<Value> = applications
            .iter()
            .map(application_json)
            .collect::<Vec<_>>();
        return Ok(Json(Value::Array(list)).into_response());
    }

    let rows = applications
        .iter()
        .map(|application| {
            format!(
                "<tr><td><a href=\"/id/oauth/applications/{id}\">{name}</a></td>\
                 <td>{redirect_uri}</td><td>{uid}</td></tr>",
                id = application.id,
                name = escape_html(&application.name),
                redirect_uri = escape_html(&application.redirect_uri),
                uid = escape_html(&application.uid)
            )
        })
        .collect::<String>();
    let body = format!(
        "<h1>Your Applications</h1>\
         <p><a href=\"/id/oauth/applications\">Refresh</a></p>\
         <table><thead><tr><th>Name</th><th>Callback URL</th><th>Application Id</th></tr>\
         </thead><tbody>{rows}</tbody></table>\
         <h2>New Application</h2>\
         <form method=\"post\" action=\"/id/oauth/applications\">\
         <label for=\"name\">Name</label><input id=\"name\" name=\"name\" required>\
         <label for=\"redirect_uri\">Callback URL</label>\
         <textarea id=\"redirect_uri\" name=\"redirect_uri\" required></textarea>\
         <button type=\"submit\">Submit</button></form>"
    );
    Ok(page("Your Applications", &body).into_response())
}

pub async fn create_application(
    State(state): State<OAuthState>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, OAuthError> {
    require_admin(&state, &headers).await?;
    let params = parse_params(request).await?;
    let name = params
        .get("name")
        .ok_or_else(|| OAuthError::invalid_request("name is required"))?;
    let redirect_uri = params
        .get("redirect_uri")
        .ok_or_else(|| OAuthError::invalid_request("redirect_uri is required"))?;
    let scopes = params.get("scopes").unwrap_or_default();
    let confidential = params
        .get("confidential")
        .map(|value| value != "false" && value != "0")
        .unwrap_or(true);

    let application = state
        .store
        .create_oauth_application(&name, &redirect_uri, &scopes, confidential)
        .await?;

    if params.is_json() || wants_json(&headers) {
        return Ok((StatusCode::CREATED, Json(application_json(&application))).into_response());
    }
    Ok(redirect(&format!(
        "/id/oauth/applications/{}",
        application.id
    )))
}

pub async fn get_application(
    State(state): State<OAuthState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Response, OAuthError> {
    require_admin(&state, &headers).await?;
    let application = state
        .store
        .find_oauth_application(id)
        .await?
        .ok_or_else(|| OAuthError::not_found(format!("oauth application {id}")))?;

    if wants_json(&headers) {
        return Ok(Json(application_json(&application)).into_response());
    }

    let body = format!(
        "<h1>Application: {name}</h1>\
         <h4>Application Id:</h4><p><code id=\"application_id\">{uid}</code></p>\
         <h4>Secret:</h4><p><code id=\"secret\">{secret}</code></p>\
         <h4>Callback URLs:</h4><p><code>{redirect_uri}</code></p>\
         <h2>Edit</h2>\
         <form method=\"post\" action=\"/id/oauth/applications/{id}\">\
         <input type=\"hidden\" name=\"_method\" value=\"PUT\">\
         <label for=\"name\">Name</label>\
         <input id=\"name\" name=\"name\" value=\"{name_value}\" required>\
         <label for=\"redirect_uri\">Callback URL</label>\
         <textarea id=\"redirect_uri\" name=\"redirect_uri\" required>{redirect_uri}</textarea>\
         <button type=\"submit\">Save</button></form>",
        name = escape_html(&application.name),
        name_value = escape_html(&application.name),
        uid = escape_html(&application.uid),
        secret = escape_html(&application.secret),
        redirect_uri = escape_html(&application.redirect_uri),
        id = application.id
    );
    Ok(page("Application", &body).into_response())
}

pub async fn update_application(
    State(state): State<OAuthState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
    request: Request,
) -> Result<Response, OAuthError> {
    require_admin(&state, &headers).await?;
    let params = parse_params(request).await?;
    let name = params.get("name");
    let redirect_uri = params.get("redirect_uri");
    if name.is_none() && redirect_uri.is_none() {
        return Err(OAuthError::invalid_request(
            "name or redirect_uri is required",
        ));
    }

    let application = state
        .store
        .update_oauth_application(id, name.as_deref(), redirect_uri.as_deref())
        .await?;

    if params.is_json() || wants_json(&headers) {
        return Ok(Json(application_json(&application)).into_response());
    }
    Ok(redirect(&format!("/id/oauth/applications/{id}")))
}

pub async fn delete_application(
    State(state): State<OAuthState>,
    Path(id): Path<i64>,
    headers: HeaderMap,
) -> Result<Response, OAuthError> {
    require_admin(&state, &headers).await?;
    state.store.delete_oauth_application(id).await?;
    if wants_json(&headers) {
        return Ok(Json(json!({})).into_response());
    }
    Ok(redirect("/id/oauth/applications"))
}

fn application_json(application: &OAuthApplication) -> Value {
    json!({
        "id": application.id,
        "name": application.name,
        "uid": application.uid,
        "secret": application.secret,
        "redirect_uri": application.redirect_uri,
        "scopes": application.scopes,
        "confidential": application.confidential,
        "created_at": application.created_at,
        "updated_at": application.updated_at
    })
}
