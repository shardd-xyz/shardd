use serde::Deserialize;
use thiserror::Error;

/// Everything a shardd SDK call can fail with.
#[derive(Debug, Error)]
pub enum ShardError {
    /// 400 — the server rejected the request shape (missing nonce,
    /// oversized note, invalid amount).
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// 401 — the API key is missing, malformed, or revoked.
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// 403 — the API key is valid but lacks permission for this
    /// bucket / action, or the account is frozen.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// 404 — bucket, account, or route not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// 422 — the debit would exceed available balance plus any
    /// `max_overdraft`. `available_balance` tells you how short you are.
    #[error("insufficient funds: balance={balance}, available={available_balance}")]
    InsufficientFunds {
        balance: i64,
        available_balance: i64,
        /// The `max_overdraft` the request opted into (0 if none).
        limit: i64,
    },

    /// 402 — the account is out of credits and no top-up plan is active.
    #[error("payment required")]
    PaymentRequired,

    /// 503/504, or timeout/connection failure after failover was exhausted.
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    /// Client-side timeout. Retries could succeed.
    #[error("request timed out")]
    RequestTimeout,

    /// The response body didn't match the expected shape.
    #[error("decode error: {0}")]
    Decode(String),

    /// Transport-level failure (DNS, TLS, connect, etc.) that exhausted
    /// every candidate edge.
    #[error("network error: {0}")]
    Network(String),
}

impl ShardError {
    /// `true` for errors where retrying (on the same edge, after a
    /// backoff, or on a different edge) might succeed. Applied by the
    /// internal failover loop; also useful for app-level retry policy.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ShardError::ServiceUnavailable(_) | ShardError::RequestTimeout | ShardError::Network(_)
        )
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct GatewayErrorBody {
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub balance: Option<i64>,
    #[serde(default)]
    pub available_balance: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

impl GatewayErrorBody {
    pub fn message_text(&self) -> String {
        self.error
            .clone()
            .or_else(|| self.message.clone())
            .unwrap_or_else(|| "unknown error".to_string())
    }
}

pub(crate) fn from_status(status: u16, body: Option<GatewayErrorBody>) -> ShardError {
    let text = body
        .as_ref()
        .map(|b| b.message_text())
        .unwrap_or_else(|| format!("HTTP {status}"));
    match status {
        400 => ShardError::InvalidInput(text),
        401 => ShardError::Unauthorized(text),
        402 => ShardError::PaymentRequired,
        403 => ShardError::Forbidden(text),
        404 => ShardError::NotFound(text),
        422 => {
            let b = body.unwrap_or(GatewayErrorBody {
                error: None,
                message: None,
                balance: None,
                available_balance: None,
                limit: None,
            });
            ShardError::InsufficientFunds {
                balance: b.balance.unwrap_or(0),
                available_balance: b.available_balance.unwrap_or(0),
                limit: b.limit.unwrap_or(0),
            }
        }
        503 | 504 => ShardError::ServiceUnavailable(text),
        _ => ShardError::Decode(format!("unexpected HTTP {status}: {text}")),
    }
}
