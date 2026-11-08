use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use microwave_objectstore::ObjectStoreError;
use microwave_store::StoreError;
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
    /// When set, the response body is the upstream object form
    /// `{"error": message, "not_found": kind}` instead of `{"error": [message]}`.
    pub not_found_kind: Option<String>,
    /// When set, `error` is emitted as a plain string rather than a one-element
    /// array. Some upstream resources (notably key self-modification) do this.
    pub error_as_string: bool,
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    pub fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "Failed to authenticate".into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    pub fn unauthorized_with(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    pub fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: "missing permission".into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    pub fn forbidden_with(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    /// A forbidden response whose `error` member is a plain string.
    pub fn forbidden_string(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
            not_found_kind: None,
            error_as_string: true,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    /// Build the upstream `not_found_ejson/2` body used by resources such as
    /// the principal endpoint.
    pub fn not_found_object(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
            not_found_kind: Some(kind.into()),
            error_as_string: false,
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: message.into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    pub fn method_not_allowed(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::METHOD_NOT_ALLOWED,
            message: message.into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    /// A `410 Gone` response whose `error` member is a plain string, matching
    /// the removed analytics/controls endpoint.
    pub fn gone(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::GONE,
            message: message.into(),
            not_found_kind: None,
            error_as_string: true,
        }
    }

    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: message.into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: message.into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    pub fn gateway_timeout(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::GATEWAY_TIMEOUT,
            message: message.into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    pub fn precondition_failed(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PRECONDITION_FAILED,
            message: message.into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }

    pub fn internal() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal server error".into(),
            not_found_kind: None,
            error_as_string: false,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = match self.not_found_kind {
            Some(kind) => json!({ "error": self.message, "not_found": kind }),
            None if self.error_as_string => json!({ "error": self.message }),
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
            StoreError::Quota { kind, limit } => {
                Self::forbidden_with(format!("{kind} limit of {limit} reached"))
            }
            other => {
                tracing::error!(error = %other, "store error");
                Self::internal()
            }
        }
    }
}

/// Turn an object-store failure into a logged `500`.
///
/// `operation` is the machine-readable operation for the log line, `message`
/// the human-readable action. The routed store already logged the error
/// itself, so this line adds the action once instead of repeating it. The
/// object key layout is derivable from the org id, so the checksum (and
/// therefore the key) never appears here.
pub(crate) fn object_store_failure(
    org_id: &str,
    operation: &'static str,
    _error: ObjectStoreError,
    message: &'static str,
) -> ApiError {
    tracing::error!(org_id = %org_id, operation = operation, "{message}");
    ApiError::internal()
}

#[cfg(test)]
mod tests {
    use super::object_store_failure;
    use microwave_objectstore::ObjectStoreError;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Capture;

        fn make_writer(&'a self) -> Self::Writer {
            Capture(self.0.clone())
        }
    }

    /// The routed store logs the backend error; this line only adds the
    /// action once, so neither the message nor the error repeats.
    #[test]
    fn object_store_failure_logs_the_action_without_repeating_the_error() {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(Capture(buffer.clone()))
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            object_store_failure(
                "acme",
                "put",
                ObjectStoreError::HttpStatus(503),
                "blob upload failed",
            );
        });

        let log = String::from_utf8_lossy(&buffer.lock().unwrap()).into_owned();
        assert!(log.contains("acme"), "{log}");
        assert!(log.contains("put"), "{log}");
        assert_eq!(log.matches("blob upload failed").count(), 1, "{log}");
        assert!(!log.contains("object store returned status 503"), "{log}");
    }
}
