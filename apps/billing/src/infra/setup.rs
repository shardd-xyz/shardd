use std::sync::Arc;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use crate::{
    adapters::http::app_state::AppState,
    application::billing::BillingRepo,
    infra::{config::BillingConfig, postgres_persistence},
};

pub async fn init_app_state() -> anyhow::Result<AppState> {
    let config = BillingConfig::from_env();

    let persistence = Arc::new(postgres_persistence(&config.database_url).await?);
    let billing_repo = persistence as Arc<dyn BillingRepo>;

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;

    let stripe_client = stripe::Client::new(
        secrecy::ExposeSecret::expose_secret(&config.stripe_secret_key).to_string(),
    );

    Ok(AppState {
        config: Arc::new(config),
        billing_repo,
        http_client,
        stripe_client,
    })
}

pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "shardd_billing=info,tower_http=info".into());

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false).with_level(true).compact())
        .try_init()
        .ok();
}
