use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("Too many requests. Please slow down.")]
    RateLimited,

    #[error("Invalid credentials")]
    InvalidCredentials,

    #[error("Forbidden")]
    Forbidden,

    #[error("Not found")]
    NotFound,

    #[error("Account frozen")]
    AccountFrozen,

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Conflict: {0}")]
    Conflict(String),

    #[error("Internal error: {0}")]
    Internal(String),
}

#[derive(Clone, Copy, Debug)]
pub enum ErrorCode {
    DatabaseError,
    InvalidCredentials,
    Forbidden,
    NotFound,
    AccountFrozen,
    RateLimited,
    InvalidInput,
    Conflict,
    InternalError,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::DatabaseError => "DATABASE_ERROR",
            ErrorCode::InvalidCredentials => "INVALID_CREDENTIALS",
            ErrorCode::Forbidden => "FORBIDDEN",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::AccountFrozen => "ACCOUNT_FROZEN",
            ErrorCode::RateLimited => "RATE_LIMITED",
            ErrorCode::InvalidInput => "INVALID_INPUT",
            ErrorCode::Conflict => "CONFLICT",
            ErrorCode::InternalError => "INTERNAL_ERROR",
        }
    }
}

pub type AppResult<T> = Result<T, AppError>;
