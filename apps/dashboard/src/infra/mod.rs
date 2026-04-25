use crate::{adapters::persistence::PostgresPersistence, infra::db::init_db};

pub mod app;
pub mod cli_auth;
pub mod config;
pub mod db;
pub mod magic_links;
pub mod rate_limit;
pub mod setup;

pub async fn postgres_persistence(database_url: &str) -> anyhow::Result<PostgresPersistence> {
    let pool = init_db(database_url).await?;
    let persistence = PostgresPersistence::new(pool);
    Ok(persistence)
}
