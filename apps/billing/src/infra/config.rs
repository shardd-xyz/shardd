use std::net::SocketAddr;

use axum::http::HeaderValue;
use env_helpers::{get_env, get_env_default};
use secrecy::SecretString;
use url::Url;

pub struct BillingConfig {
    pub bind_addr: SocketAddr,
    pub database_url: String,
    pub jwt_secret: SecretString,
    pub cors_origin: HeaderValue,
    pub app_origin: Url,
    pub stripe_secret_key: SecretString,
    pub stripe_webhook_secret: SecretString,
    pub gateway_url: String,
    pub gateway_machine_auth_secret: SecretString,
    pub billing_internal_secret: SecretString,
    pub resend_api_key: SecretString,
    pub email_from: String,
}

impl BillingConfig {
    pub fn from_env() -> Self {
        Self {
            bind_addr: get_env_default("BIND_ADDR", "127.0.0.1:3002".parse().unwrap()),
            database_url: get_env("DATABASE_URL"),
            jwt_secret: SecretString::new(get_env::<String>("JWT_SECRET").into()),
            cors_origin: get_env_default("CORS_ORIGIN", String::from("http://localhost:3000"))
                .parse()
                .expect("CORS_ORIGIN must be a valid header value"),
            app_origin: get_env("APP_ORIGIN"),
            stripe_secret_key: SecretString::new(get_env::<String>("STRIPE_SECRET_KEY").into()),
            stripe_webhook_secret: SecretString::new(
                get_env::<String>("STRIPE_WEBHOOK_SECRET").into(),
            ),
            gateway_url: get_env("GATEWAY_URL"),
            gateway_machine_auth_secret: SecretString::new(
                get_env::<String>("GATEWAY_MACHINE_AUTH_SECRET").into(),
            ),
            billing_internal_secret: SecretString::new(
                get_env::<String>("BILLING_INTERNAL_SECRET").into(),
            ),
            resend_api_key: SecretString::new(get_env::<String>("RESEND_API_KEY").into()),
            email_from: get_env_default("EMAIL_FROM", "noreply@shardd.xyz".to_string()),
        }
    }
}
