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
        AppError::Database(value.to_string())
    }
}
