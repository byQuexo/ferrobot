use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use ferrobot_sandbox::SandboxError;
use ferrobot_store::StoreError;
use serde_json::json;
use thiserror::Error;

/// Errors returned by the UI API, rendered as `{"error": code, "message": ...}`.
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Conflict(String),
    #[error("sandbox error: {0}")]
    Sandbox(#[from] SandboxError),
    #[error("internal error")]
    Internal(String),
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::NotFound { .. } => Self::NotFound(e.to_string()),
            StoreError::Conflict { .. } => Self::Conflict(e.to_string()),
            StoreError::Core(core) => Self::BadRequest(core.to_string()),
            other => Self::Internal(other.to_string()),
        }
    }
}

impl From<ferrobot_core::CoreError> for ApiError {
    fn from(e: ferrobot_core::CoreError) -> Self {
        Self::BadRequest(e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = match &self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::Sandbox(SandboxError::NotImplemented { .. }) => {
                (StatusCode::NOT_IMPLEMENTED, "not_implemented")
            }
            Self::Sandbox(_) => (StatusCode::BAD_GATEWAY, "sandbox_error"),
            Self::Internal(detail) => {
                // Log the detail, never leak it to clients.
                tracing::error!(%detail, "internal API error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        };
        (
            status,
            Json(json!({ "error": code, "message": self.to_string() })),
        )
            .into_response()
    }
}
