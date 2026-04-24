use sqlx::PgPool;

pub mod billing;

#[derive(Clone)]
pub struct PostgresPersistence {
    pub(crate) pool: PgPool,
}

impl PostgresPersistence {
    pub fn new(pool: PgPool) -> Self {
        PostgresPersistence { pool }
    }
}
