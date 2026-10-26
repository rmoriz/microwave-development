//! Chef-shaped JSON extraction.
//!
//! Axum's built-in `Json` extractor rejects malformed payloads with a
//! plain-text body. Erchef (via `chef_wm_util:create_from_json/3`) always
//! answers with the EJSON error envelope, so the API uses this thin wrapper
//! wherever a request body is a JSON document.

use crate::ApiError;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::http::{header, HeaderMap, StatusCode};
use axum::Json;
use serde::de::DeserializeOwned;

/// A JSON body extractor whose rejections use the Chef error envelope.
pub struct ChefJson<T>(pub T);

impl<S, T> FromRequest<S> for ChefJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(json_rejection(rejection)),
        }
    }
}

/// A JSON body extractor for resources upstream guards with Webmachine's
/// content negotiation (`chef_wm_named_environment`, `chef_wm_roles`, ...).
///
/// A request that cannot accept `application/json`, or that submits a
/// non-JSON content type, is answered with `406 Not Acceptable`. Once the
/// media types agree, malformed payloads keep the usual `400 {"error":
/// ["invalid JSON"]}` envelope.
pub struct NegotiatedChefJson<T>(pub T);

impl<S, T> FromRequest<S> for NegotiatedChefJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        if !accepts_json(request.headers()) || !is_json_content_type(request.headers()) {
            return Err(not_acceptable());
        }
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(json_rejection(rejection)),
        }
    }
}

/// Translate axum's `Json` rejection into the upstream error shape.
///
/// Malformed payloads, bodies that do not match the expected JSON type, and
/// missing/foreign content types all become `400 {"error":["invalid JSON"]}`.
pub fn json_rejection(rejection: JsonRejection) -> ApiError {
    match rejection {
        JsonRejection::JsonSyntaxError(_)
        | JsonRejection::JsonDataError(_)
        | JsonRejection::MissingJsonContentType(_) => ApiError::bad_request("invalid JSON"),
        rejection => ApiError::bad_request(rejection.body_text()),
    }
}

fn not_acceptable() -> ApiError {
    ApiError {
        status: StatusCode::NOT_ACCEPTABLE,
        message: "Not Acceptable".into(),
        not_found_kind: None,
        error_as_string: false,
    }
}

/// Whether the request explicitly accepts a JSON representation.
///
/// A missing `Accept` header means "anything", matching the Chef clients that
/// omit it. Media ranges with `q=0` are ignored, and `application/*`/`*/*`
/// concedes JSON.
fn accepts_json(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };
    if value.trim().is_empty() {
        return true;
    }
    value.split(',').any(|range| {
        let mut parts = range.split(';');
        let media_type = parts.next().unwrap_or_default().trim();
        let quality = parts
            .filter_map(|parameter| parameter.trim().strip_prefix("q="))
            .find_map(|quality| quality.trim().parse::<f32>().ok())
            .unwrap_or(1.0);
        quality > 0.0 && media_type_accepts_json(media_type)
    })
}

fn media_type_accepts_json(media_type: &str) -> bool {
    let media_type = media_type.to_ascii_lowercase();
    matches!(media_type.as_str(), "*/*" | "application/*")
        || media_type == "application/json"
        || media_type.ends_with("+json")
}

/// Whether the submitted entity is JSON. A missing `Content-Type` is treated
/// as incompatible, which is what Webmachine's `content_types_accepted`
/// negotiation reports for these resources.
fn is_json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let media_type = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    media_type == "application/json" || media_type.ends_with("+json")
}
