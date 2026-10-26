//! Chef-shaped JSON extraction.
//!
//! Axum's built-in `Json` extractor rejects malformed payloads with a
//! plain-text body. Erchef (via `chef_wm_util:create_from_json/3`) always
//! answers with the EJSON error envelope, so the API uses this thin wrapper
//! wherever a request body is a JSON document.

use crate::ApiError;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
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
