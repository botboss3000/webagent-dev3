//! The HTTP error every host API handler returns.
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

/// Public status and message; internal causes are logged, never returned.
#[derive(Debug)]
pub struct ApiError(pub StatusCode, pub String);

/// Result of a JSON API handler.
pub type Api = Result<Json<Value>, ApiError>;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        if error
            .downcast_ref::<webagent_chat::session::ArchivedSession>()
            .is_some()
        {
            return ApiError(StatusCode::NOT_FOUND, "Session not found".into());
        }
        tracing::error!(
            cause = %storage_cause(&error),
            "Request storage or execution operation failed"
        );
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Operation failed; check development logs".into(),
        )
    }
}

/// The database or I/O cause in `error`'s chain, which names no user data. Other causes can
/// quote request content, so only their absence is logged.
pub(crate) fn storage_cause(error: &anyhow::Error) -> String {
    for cause in error.chain() {
        if let Some(error) = cause.downcast_ref::<sqlx::Error>() {
            return match error {
                sqlx::Error::Database(_)
                | sqlx::Error::Io(_)
                | sqlx::Error::PoolTimedOut
                | sqlx::Error::PoolClosed
                | sqlx::Error::WorkerCrashed => error.to_string(),
                _ => "other database error".into(),
            };
        }
        if let Some(error) = cause.downcast_ref::<std::io::Error>() {
            return format!(
                "I/O {:?} (os error {:?})",
                error.kind(),
                error.raw_os_error()
            );
        }
    }
    "not a database or I/O error".into()
}

// Module error conversions live here: the orphan rule allows them only in the
// crate that owns `ApiError` or the one that owns the module error.
impl From<webagent_models::api::Error> for ApiError {
    fn from(error: webagent_models::api::Error) -> Self {
        match error {
            webagent_models::api::Error::Public(status, message) => Self(status, message),
            webagent_models::api::Error::Internal(error) => error.into(),
        }
    }
}

impl From<webagent_accounts::api::Error> for ApiError {
    fn from(error: webagent_accounts::api::Error) -> Self {
        match error {
            webagent_accounts::api::Error::Public(status, message) => Self(status, message),
            webagent_accounts::api::Error::Storage(error) => Self::from(error),
        }
    }
}

/// 400 with a public message.
pub fn bad(message: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.into())
}

/// 409 with a public message.
pub fn conflict(message: &str) -> ApiError {
    ApiError(StatusCode::CONFLICT, message.into())
}
