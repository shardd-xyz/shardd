use sqlx::PgPool;

use crate::app_error::AppError;

pub mod audit;
pub mod buckets_registry;
pub mod developer_auth;
pub mod user;

#[derive(Clone)]
pub struct PostgresPersistence {
    pub(crate) pool: PgPool,
}

impl PostgresPersistence {
    pub fn new(pool: PgPool) -> Self {
        PostgresPersistence { pool }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(value: sqlx::Error) -> Self {
        // Postgres unique_violation (SQLSTATE 23505) → 409 with a
        // friendly message. Caller code shouldn't have to special-case
        // this everywhere it inserts a row that has a UNIQUE; the
        // dashboard UI already maps `Conflict` to a "Name taken"
        // notice, and the CLI surfaces it verbatim.
        if let sqlx::Error::Database(db_err) = &value
            && db_err.code().as_deref() == Some("23505")
        {
            let constraint = db_err.constraint().unwrap_or("");
            let msg = if constraint == "developer_api_keys_user_active_name_unique" {
                "An active API key with that name already exists. Pick another name or revoke the existing key.".to_string()
            } else {
                format!("Conflict ({constraint}): {}", db_err.message())
            };
            return AppError::Conflict(msg);
        }
        AppError::Database(value.to_string())
    }
}
