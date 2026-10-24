use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use microwave_store::StoreError;
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
    /// When set, the response body is the upstream object form
    /// `{"error": message, "not_found": kind}` instead of `{"error": [message]}`.
    pub not_found_kind: Option<String>,
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            not_found_kind: None,
        }
    }

    pub fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "Failed to authenticate".into(),
            not_found_kind: None,
        }
    }

    pub fn unauthorized_with(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
            not_found_kind: None,
        }
    }

    pub fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: "missing permission".into(),
            not_found_kind: None,
        }
    }

    pub fn forbidden_with(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
            not_found_kind: None,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
            not_found_kind: None,
        }
    }

    /// Build the upstream `not_found_ejson/2` body used by resources such as
    /// the principal endpoint.
    pub fn not_found_object(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
            not_found_kind: Some(kind.into()),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
            not_found_kind: None,
        }
    }

    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: message.into(),
        }
    }

    pub fn method_not_allowed(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::METHOD_NOT_ALLOWED,
            message: message.into(),
            not_found_kind: None,
        }
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: message.into(),
            not_found_kind: None,
        }
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
            not_found_kind: None,
        }
    }

    pub fn precondition_failed(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PRECONDITION_FAILED,
            message: message.into(),
            not_found_kind: None,
        }
    }

    pub fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal server error".into(),
            not_found_kind: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = match self.not_found_kind {
            Some(kind) => json!({ "error": self.message, "not_found": kind }),
            None => json!({ "error": [self.message] }),
        };
        (self.status, axum::Json(body)).into_response()
    }
}

impl From<crate::search::SearchError> for ApiError {
    fn from(error: crate::search::SearchError) -> Self {
        match error {
            crate::search::SearchError::BadRequest => Self::bad_request("invalid query"),
            other => {
                tracing::error!(error = %other, "search backend error");
                Self::internal()
            }
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Conflict(message) => Self::conflict(message),
            StoreError::NotFound(message) => Self::not_found(message),
            StoreError::Invalid(message) => Self::bad_request(message),
            StoreError::Forbidden(message) => Self::forbidden_with(message),
            other => {
                tracing::error!(error = %other, "store error");
                Self::internal()
            }
        }
    }
}
