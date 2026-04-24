use std::sync::Arc;

use crate::{application::billing::BillingRepo, infra::config::BillingConfig};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<BillingConfig>,
    pub billing_repo: Arc<dyn BillingRepo>,
    pub http_client: reqwest::Client,
    pub stripe_client: stripe::Client,
}
