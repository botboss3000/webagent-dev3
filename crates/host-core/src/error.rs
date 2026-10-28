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
            .downcast_ref::<webagent_chat::session::RecycledSession>()
            .is_some()
        {
            return ApiError(StatusCode::NOT_FOUND, "Session not found".into());
        }
        tracing::error!("Request storage or execution operation failed");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Operation failed; check development logs".into(),
        )
    }
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
