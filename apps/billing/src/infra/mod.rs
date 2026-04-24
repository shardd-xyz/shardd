use crate::adapters::persistence::PostgresPersistence;

pub mod app;
pub mod config;
pub mod db;
pub mod notifications;
pub mod setup;

pub async fn postgres_persistence(database_url: &str) -> anyhow::Result<PostgresPersistence> {
    let pool = db::init_db(database_url).await?;
    Ok(PostgresPersistence::new(pool))
}
