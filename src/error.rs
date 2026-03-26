use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("storage: {0}")]
    Storage(#[from] std::io::Error),

    #[error("json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("network: {0}")]
    Network(#[from] reqwest::Error),

    #[error("{0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "error": self.to_string() });
        (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(body)).into_response()
    }
}
