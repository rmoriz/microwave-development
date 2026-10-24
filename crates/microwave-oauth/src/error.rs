use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// An OAuth2-style error response.
#[derive(Debug)]
pub struct OAuthError {
    pub status: StatusCode,
    pub error: &'static str,
    pub description: Option<String>,
}

impl OAuthError {
    pub fn new(status: StatusCode, error: &'static str, description: impl Into<String>) -> Self {
        Self {
            status,
            error,
            description: Some(description.into()),
        }
    }

    pub fn invalid_request(description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_request", description)
    }

    pub fn invalid_client(description: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "invalid_client", description)
    }

    pub fn invalid_grant(description: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid_grant", description)
    }

    pub fn unauthorized(description: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", description)
    }

    pub fn forbidden(description: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", description)
    }

    pub fn not_found(description: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", description)
    }

    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "internal server error",
        )
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.error });
        if let Some(description) = self.description {
            body["error_description"] = json!(description);
        }
        (self.status, axum::Json(body)).into_response()
    }
}

impl From<microwave_store::StoreError> for OAuthError {
    fn from(error: microwave_store::StoreError) -> Self {
        match error {
            microwave_store::StoreError::Conflict(message) => Self::invalid_request(message),
            microwave_store::StoreError::NotFound(message) => Self::not_found(message),
            microwave_store::StoreError::Invalid(message) => Self::invalid_request(message),
            microwave_store::StoreError::Forbidden(message) => Self::forbidden(message),
            other => {
                tracing::error!(error = %other, "identity store error");
                Self::internal()
            }
        }
    }
}
