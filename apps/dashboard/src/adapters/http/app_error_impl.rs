use crate::app_error::{AppError, ErrorCode};
use axum::Json;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!(error = ?self, "Request failed");

        match self {
            AppError::Database(_) => error_resp(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::DatabaseError,
                None,
            ),
            AppError::InvalidCredentials => error_resp(
                StatusCode::UNAUTHORIZED,
                ErrorCode::InvalidCredentials,
                None,
            ),
            AppError::Forbidden => error_resp(StatusCode::FORBIDDEN, ErrorCode::Forbidden, None),
            AppError::NotFound => error_resp(StatusCode::NOT_FOUND, ErrorCode::NotFound, None),
            AppError::AccountFrozen => {
                error_resp(StatusCode::FORBIDDEN, ErrorCode::AccountFrozen, None)
            }
            AppError::RateLimited => {
                error_resp(StatusCode::TOO_MANY_REQUESTS, ErrorCode::RateLimited, None)
            }
            AppError::InvalidInput(message) => error_resp(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidInput,
                Some(message),
            ),
            AppError::Conflict(message) => {
                error_resp(StatusCode::CONFLICT, ErrorCode::Conflict, Some(message))
            }
            AppError::Internal(_) => error_resp(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::InternalError,
                None,
            ),
        }
    }
}

fn error_resp(status: StatusCode, code: ErrorCode, message: Option<String>) -> Response {
    let mut body = serde_json::json!({ "code": code.as_str() });
    // Include the operator-supplied detail only for user-actionable errors
    // (InvalidInput/Conflict). Database/Internal messages leak implementation
    // details and stay server-side only — the frontend shows a generic copy.
    if let Some(msg) = message {
        body["message"] = serde_json::Value::String(msg);
    }
    (status, Json(body)).into_response()
}
