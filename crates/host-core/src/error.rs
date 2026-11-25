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
        let retryable = matches!(
            self.0,
            StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS
        );
        let mut response = (self.0, Json(json!({"error": self.1}))).into_response();
        if retryable {
            // A capacity refusal is temporary: tell the client when to come back.
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("2"),
            );
        }
        response
    }
}

/// Every temporary capacity refusal in the error chain, where the refused step wrote nothing:
/// the account-database slot budget ([`webagent_storage::pool::Busy`], refused before the
/// database opens), a connection pool that timed out handing out a connection, or a thread that
/// could not be created ("Resource temporarily unavailable", os error 11). These are answered
/// 503 with a retry hint, never 500. A database that stayed locked past its busy timeout is not
/// in this list: it can fail a handler after earlier statements committed, and a client repeating
/// such a request could repeat those effects, so it stays a 500 (API-SLOT-WAIT).
fn retryable_capacity(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| match cause.downcast_ref::<sqlx::Error>() {
            Some(sqlx::Error::PoolTimedOut) => true,
            Some(sqlx::Error::Io(io)) => io.raw_os_error() == Some(11),
            _ => false,
        })
        || error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.raw_os_error() == Some(11))
        })
        || error
            .downcast_ref::<webagent_storage::pool::Busy>()
            .is_some()
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        if error
            .downcast_ref::<webagent_chat::session::ArchivedSession>()
            .is_some()
        {
            return ApiError(StatusCode::NOT_FOUND, "Session not found".into());
        }
        if retryable_capacity(&error) {
            tracing::warn!(
                cause = %storage_cause(&error),
                "Request refused: capacity is busy, retryable"
            );
            return Self(
                StatusCode::SERVICE_UNAVAILABLE,
                "The server is busy; retry in a few seconds".into(),
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    /// Every temporary capacity failure is retried, and an ordinary failure is not.
    fn capacity_refusals_are_retryable_and_real_faults_are_not() {
        assert!(retryable_capacity(&anyhow::Error::new(
            webagent_storage::pool::Busy
        )));
        assert!(retryable_capacity(&anyhow::Error::from(
            std::io::Error::from_raw_os_error(11)
        )));
        assert!(!retryable_capacity(&anyhow::Error::from(
            std::io::Error::from_raw_os_error(2)
        )));
        assert!(!retryable_capacity(&anyhow::anyhow!(
            "not a database or I/O error"
        )));
        // A pool that handed out no connection refused before any statement ran.
        assert!(retryable_capacity(&anyhow::Error::from(
            sqlx::Error::PoolTimedOut
        )));
        // A closed pool is a fault, not capacity, and stays a 500.
        assert!(!retryable_capacity(&anyhow::Error::from(
            sqlx::Error::PoolClosed
        )));
    }

    #[test]
    /// A capacity refusal answers 503 with a retry hint; any other failure stays a 500.
    fn a_busy_server_answers_503_and_a_fault_answers_500() {
        let busy = ApiError::from(anyhow::Error::new(webagent_storage::pool::Busy)).into_response();
        assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            busy.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
            Some("2")
        );
        let fault = ApiError::from(anyhow::anyhow!("not a database or I/O error")).into_response();
        assert_eq!(fault.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(fault
            .headers()
            .get(axum::http::header::RETRY_AFTER)
            .is_none());
    }
}
