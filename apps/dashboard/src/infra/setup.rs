use crate::{
    adapters::{email::resend::ResendEmailSender, http::app_state::AppState},
    infra::{
        cli_auth::CliAuthStore, config::AppConfig, magic_links::MagicLinkStore,
        postgres_persistence, rate_limit::RateLimiter,
    },
    use_cases::{
        audit::AuditLogRepo,
        buckets_registry::BucketRegistry,
        cli_auth::{CliAuthStore as CliAuthStoreTrait, CliAuthUseCases},
        developer_auth::{DeveloperAuthRepo, DeveloperAuthUseCases},
        user::{AuthUseCases, UserRepo},
    },
};
use std::sync::Arc;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

pub async fn init_app_state() -> anyhow::Result<AppState> {
    let config = AppConfig::from_env();
    // Stale pooled connections were the source of a long-running 500
    // on /api/developer/buckets — every request waited the full 5 s
    // per-edge timeout × 3 edges before giving up. Short pool-idle
    // timeout + connect-timeout + TCP keepalive poison dead sockets
    // before reqwest tries to reuse them.
    let edge_http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .connect_timeout(std::time::Duration::from_secs(2))
        .pool_idle_timeout(std::time::Duration::from_secs(10))
        .tcp_keepalive(std::time::Duration::from_secs(20))
        .build()?;

    let postgres_arc = Arc::new(postgres_persistence(&config.database_url).await?);

    let rate_limiter = Arc::new(
        RateLimiter::new(
            &config.redis_url,
            config.rate_limit_window_secs,
            config.rate_limit_per_ip,
            config.rate_limit_per_email,
        )
        .await?,
    );

    let magic_links = Arc::new(MagicLinkStore::new(&config.redis_url).await?);

    let email = Arc::new(ResendEmailSender::new(
        config.resend_api_key.clone(),
        config.email_from.clone(),
    ));

    let user_repo_arc = postgres_arc.clone() as Arc<dyn UserRepo>;
    let audit_repo_arc = postgres_arc.clone() as Arc<dyn AuditLogRepo>;
    let developer_auth_repo_arc = postgres_arc.clone() as Arc<dyn DeveloperAuthRepo>;
    let bucket_registry_arc = postgres_arc.clone() as Arc<dyn BucketRegistry>;

    let auth_use_cases = AuthUseCases::new(
        user_repo_arc.clone(),
        magic_links,
        email.clone(),
        config.app_origin.to_string(),
    );
    let developer_auth_use_cases = Arc::new(DeveloperAuthUseCases::new(
        developer_auth_repo_arc.clone(),
        config.machine_auth_positive_cache_ttl_ms,
    ));

    let cli_auth_store =
        Arc::new(CliAuthStore::new(&config.redis_url).await?) as Arc<dyn CliAuthStoreTrait>;
    let cli_auth_use_cases = Arc::new(CliAuthUseCases::new(
        cli_auth_store,
        developer_auth_use_cases.clone(),
        user_repo_arc.clone(),
        config.app_origin.to_string(),
    ));

    Ok(AppState {
        config: Arc::new(config),
        edge_http,
        auth_use_cases: Arc::new(auth_use_cases),
        developer_auth_use_cases,
        cli_auth_use_cases,
        user_repo: user_repo_arc,
        audit_repo: audit_repo_arc,
        developer_auth_repo: developer_auth_repo_arc,
        bucket_registry: bucket_registry_arc,
        rate_limiter,
    })
}

pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "shardd_dashboard=info,tower_http=info".into());

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false).with_level(true).compact())
        .try_init()
        .ok();
}
