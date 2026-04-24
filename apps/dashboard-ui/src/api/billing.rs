use crate::api::{ApiError, api_get, api_post};
use serde::{Deserialize, Serialize};

#[allow(dead_code)]
#[derive(Clone, Debug, Deserialize)]
pub struct BillingStatus {
    pub plan_name: String,
    pub plan_slug: String,
    pub monthly_credits: i64,
    pub credit_balance: i64,
    pub subscription_status: String,
    pub period_start: Option<String>,
    pub period_end: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BillingPlan {
    pub slug: String,
    pub name: String,
    pub monthly_credits: i64,
    pub price_cents: i32,
    pub annual_price_cents: i32,
}

#[derive(Deserialize)]
struct UrlResponse {
    pub url: String,
}

#[derive(Serialize)]
struct CheckoutBody {
    plan_slug: String,
    annual: bool,
}

pub async fn status() -> Result<BillingStatus, ApiError> {
    api_get("/api/billing/status").await
}

pub async fn plans() -> Result<Vec<BillingPlan>, ApiError> {
    api_get("/api/billing/plans").await
}

pub async fn checkout(plan_slug: &str, annual: bool) -> Result<String, ApiError> {
    let body = CheckoutBody {
        plan_slug: plan_slug.to_string(),
        annual,
    };
    let resp: UrlResponse = api_post("/api/billing/checkout", &body).await?;
    Ok(resp.url)
}

pub async fn portal() -> Result<String, ApiError> {
    let body = serde_json::json!({});
    let resp: UrlResponse = api_post("/api/billing/portal", &body).await?;
    Ok(resp.url)
}
