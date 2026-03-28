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

    #[error("insufficient funds: balance {balance} would go to {projected_balance} (limit: {limit})")]
    InsufficientFunds {
        balance: i64,
        projected_balance: i64,
        limit: i64,
    },
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, body) = match &self {
            AppError::InsufficientFunds {
                balance,
                projected_balance,
                limit,
            } => (
                StatusCode::UNPROCESSABLE_ENTITY,
                serde_json::json!({
                    "error": "insufficient_funds",
                    "message": self.to_string(),
                    "balance": balance,
                    "projected_balance": projected_balance,
                    "limit": limit,
                }),
            ),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({ "error": self.to_string() }),
            ),
        };
        (status, axum::Json(body)).into_response()
    }
}
