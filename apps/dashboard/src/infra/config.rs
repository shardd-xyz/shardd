use std::net::SocketAddr;

use axum::http::HeaderValue;
use env_helpers::{get_env, get_env_default};
use secrecy::SecretString;
use serde::Deserialize;
use time::Duration;
use url::Url;

#[derive(Clone, Debug, Deserialize)]
pub struct PublicEdgeConfig {
    pub edge_id: String,
    pub region: String,
    pub base_url: String,
    #[serde(default)]
    pub node_id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub node_label: Option<String>,
}

pub struct AppConfig {
    pub jwt_secret: SecretString,
    pub dashboard_session_key: SecretString,
    pub access_token_ttl: Duration,
    pub refresh_token_ttl: Duration,
    pub resend_api_key: SecretString,
    pub email_from: String,
    pub app_origin: Url,
    pub cors_origin: HeaderValue,
    pub magic_link_ttl_minutes: i64,
    pub bind_addr: SocketAddr,
    pub redis_url: String,
    pub rate_limit_window_secs: u64,
    pub rate_limit_per_ip: u64,
    pub rate_limit_per_email: u64,
    pub database_url: String,
    pub admin_emails: Vec<String>,
    pub impersonation_ttl_minutes: i64,
    pub machine_auth_shared_secret: Option<SecretString>,
    pub machine_auth_positive_cache_ttl_ms: u64,
    pub public_edges: Vec<PublicEdgeConfig>,
    pub google_client_id: Option<String>,
    pub google_client_secret: Option<SecretString>,
    pub billing_base_url: String,
    pub billing_internal_secret: SecretString,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let jwt_secret: SecretString = SecretString::new(get_env::<String>("JWT_SECRET").into());
        let dashboard_session_key: SecretString =
            SecretString::new(get_env::<String>("DASHBOARD_SESSION_KEY").into());

        let refresh_token_ttl_days: i64 = get_env_default("REFRESH_TOKEN_TTL_DAYS", 30);

        let access_token_ttl_secs: i64 = get_env_default("ACCESS_TOKEN_TTL_SECS", 86_400);

        let resend_api_key: SecretString =
            SecretString::new(get_env::<String>("RESEND_API_KEY").into());
        let email_from: String = get_env("EMAIL_FROM");
        let app_origin: Url = get_env("APP_ORIGIN");
        let magic_link_ttl_minutes: i64 = get_env_default("MAGIC_LINK_TTL_MINUTES", 15);
        let cors_origin: HeaderValue =
            get_env_default("CORS_ORIGIN", String::from("http://localhost:3000"))
                .parse()
                .expect("CORS_ORIGIN must be a valid header value");

        let bind_addr: SocketAddr = get_env_default("BIND_ADDR", "127.0.0.1:3001".parse().unwrap());
        let redis_url: String = get_env_default("REDIS_URL", "redis://127.0.0.1:6379".to_string());
        let rate_limit_window_secs: u64 = get_env_default("RATE_LIMIT_WINDOW_SECS", 60);
        let rate_limit_per_ip: u64 = get_env_default("RATE_LIMIT_PER_IP", 600);
        let rate_limit_per_email: u64 = get_env_default("RATE_LIMIT_PER_EMAIL", 300);
        let database_url: String = get_env("DATABASE_URL");

        let admin_emails: Vec<String> = get_env_default("ADMIN_EMAILS", String::new())
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let impersonation_ttl_minutes: i64 = get_env_default("IMPERSONATION_TTL_MINUTES", 60);
        let machine_auth_shared_secret = std::env::var("MACHINE_AUTH_SHARED_SECRET")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| SecretString::new(value.into()));
        let machine_auth_positive_cache_ttl_ms: u64 =
            get_env_default("MACHINE_AUTH_POSITIVE_CACHE_TTL_MS", 2000);
        let public_edges = match std::env::var("SHARDD_PUBLIC_EDGES_JSON") {
            Ok(raw) if !raw.trim().is_empty() => {
                serde_json::from_str::<Vec<PublicEdgeConfig>>(&raw)
                    .expect("SHARDD_PUBLIC_EDGES_JSON must be valid JSON")
            }
            _ => Vec::new(),
        };

        Self {
            jwt_secret,
            dashboard_session_key,
            access_token_ttl: Duration::seconds(access_token_ttl_secs),
            refresh_token_ttl: Duration::days(refresh_token_ttl_days),
            resend_api_key,
            email_from,
            app_origin,
            magic_link_ttl_minutes,
            cors_origin,
            bind_addr,
            redis_url,
            rate_limit_window_secs,
            rate_limit_per_ip,
            rate_limit_per_email,
            database_url,
            admin_emails,
            impersonation_ttl_minutes,
            machine_auth_shared_secret,
            machine_auth_positive_cache_ttl_ms,
            google_client_id: std::env::var("GOOGLE_CLIENT_ID")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            google_client_secret: std::env::var("GOOGLE_CLIENT_SECRET")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(|v| SecretString::new(v.into())),
            public_edges,
            billing_base_url: get_env_default(
                "BILLING_BASE_URL",
                "http://billing:3002".to_string(),
            ),
            billing_internal_secret: SecretString::new(
                get_env::<String>("BILLING_INTERNAL_SECRET").into(),
            ),
        }
    }
}
