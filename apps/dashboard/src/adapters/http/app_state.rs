use std::sync::Arc;

use axum::extract::FromRef;

use crate::{
    infra::config::AppConfig,
    infra::rate_limit::RateLimiter,
    use_cases::{
        audit::AuditLogRepo,
        buckets_registry::BucketRegistry,
        cli_auth::CliAuthUseCases,
        developer_auth::{DeveloperAuthRepo, DeveloperAuthUseCases},
        user::{AuthUseCases, UserRepo},
    },
};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<AppConfig>,
    pub edge_http: reqwest::Client,
    pub shardd_client: shardd::Client,
    pub auth_use_cases: Arc<AuthUseCases>,
    pub developer_auth_use_cases: Arc<DeveloperAuthUseCases>,
    pub cli_auth_use_cases: Arc<CliAuthUseCases>,
    pub user_repo: Arc<dyn UserRepo>,
    pub audit_repo: Arc<dyn AuditLogRepo>,
    pub developer_auth_repo: Arc<dyn DeveloperAuthRepo>,
    pub bucket_registry: Arc<dyn BucketRegistry>,
    pub rate_limiter: Arc<RateLimiter>,
}

impl FromRef<AppState> for Arc<AuthUseCases> {
    fn from_ref(app_state: &AppState) -> Self {
        app_state.auth_use_cases.clone()
    }
}

impl FromRef<AppState> for Arc<DeveloperAuthUseCases> {
    fn from_ref(app_state: &AppState) -> Self {
        app_state.developer_auth_use_cases.clone()
    }
}
