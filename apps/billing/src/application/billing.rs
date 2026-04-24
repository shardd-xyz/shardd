use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application::app_error::AppResult;

// ---------- Domain types ----------

#[derive(Clone, Debug)]
pub struct BillingPlan {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub monthly_credits: i64,
    pub price_cents: i32,
    pub annual_price_cents: i32,
    pub stripe_price_id: Option<String>,
    pub stripe_annual_price_id: Option<String>,
    pub is_active: bool,
}

#[derive(Clone, Debug)]
pub struct Subscription {
    pub user_id: Uuid,
    pub user_email: String,
    pub plan_id: Uuid,
    pub stripe_customer_id: Option<String>,
    pub stripe_subscription_id: Option<String>,
    pub subscription_status: String,
    pub period_start: Option<chrono::DateTime<chrono::Utc>>,
    pub period_end: Option<chrono::DateTime<chrono::Utc>>,
}

// ---------- Repo trait ----------

#[async_trait]
pub trait BillingRepo: Send + Sync {
    async fn list_active_plans(&self) -> AppResult<Vec<BillingPlan>>;
    async fn get_plan(&self, id: Uuid) -> AppResult<Option<BillingPlan>>;
    async fn get_plan_by_slug(&self, slug: &str) -> AppResult<Option<BillingPlan>>;
    async fn get_plan_by_stripe_price(
        &self,
        stripe_price_id: &str,
    ) -> AppResult<Option<BillingPlan>>;

    async fn get_or_create_subscription(
        &self,
        user_id: Uuid,
        email: &str,
    ) -> AppResult<Subscription>;
    async fn get_subscription_by_stripe_customer(
        &self,
        customer_id: &str,
    ) -> AppResult<Option<Subscription>>;
    async fn get_subscription_by_stripe_subscription(
        &self,
        sub_id: &str,
    ) -> AppResult<Option<Subscription>>;
    async fn set_stripe_customer_id(&self, user_id: Uuid, customer_id: &str) -> AppResult<()>;
    async fn activate_subscription(
        &self,
        user_id: Uuid,
        stripe_subscription_id: &str,
        plan_id: Uuid,
        period_start: &Option<chrono::DateTime<chrono::Utc>>,
        period_end: &Option<chrono::DateTime<chrono::Utc>>,
    ) -> AppResult<()>;
    async fn update_subscription_status(&self, user_id: Uuid, status: &str) -> AppResult<()>;
    /// Admin-initiated plan assignment: upserts the subscription with the given
    /// plan_id + email and marks `subscription_status = 'manual'` to distinguish
    /// comped/admin-set plans from Stripe-driven ones.
    async fn set_plan_manual(
        &self,
        user_id: Uuid,
        plan_id: Uuid,
        user_email: &str,
    ) -> AppResult<()>;
    async fn update_period(
        &self,
        user_id: Uuid,
        period_start: &Option<chrono::DateTime<chrono::Utc>>,
        period_end: &Option<chrono::DateTime<chrono::Utc>>,
    ) -> AppResult<()>;
    async fn cancel_subscription(&self, user_id: Uuid, free_plan_id: Uuid) -> AppResult<()>;

    async fn is_stripe_event_processed(&self, event_id: &str) -> AppResult<bool>;
    async fn mark_stripe_event_processed(&self, event_id: &str, event_type: &str) -> AppResult<()>;

    async fn list_all_subscriptions(&self) -> AppResult<Vec<Subscription>>;
    async fn was_notification_sent(&self, user_id: Uuid, threshold: &str) -> AppResult<bool>;
    async fn mark_notification_sent(&self, user_id: Uuid, threshold: &str) -> AppResult<()>;
    async fn clear_notifications(&self, user_id: Uuid) -> AppResult<()>;
}

// ---------- Mesh client for ledger operations ----------

/// Talks directly to a gateway's HTTP API for billing bucket operations.
pub struct MeshClient<'a> {
    gateway_url: &'a str,
    machine_secret: &'a str,
    http: &'a reqwest::Client,
}

#[derive(Deserialize)]
struct AccountBalance {
    balance: i64,
}

#[derive(Deserialize)]
struct BalancesResponse {
    accounts: Vec<AccountBalance>,
}

#[derive(Serialize)]
struct CreateEventPayload {
    bucket: String,
    account: String,
    amount: i64,
    note: String,
    idempotency_nonce: String,
}

impl<'a> MeshClient<'a> {
    pub fn new(gateway_url: &'a str, machine_secret: &'a str, http: &'a reqwest::Client) -> Self {
        Self {
            gateway_url,
            machine_secret,
            http,
        }
    }

    fn billing_bucket(user_id: Uuid) -> String {
        format!("__billing__{user_id}")
    }

    pub async fn get_billing_balance(&self, user_id: Uuid) -> AppResult<i64> {
        let bucket = Self::billing_bucket(user_id);
        let url = format!(
            "{}/internal/billing/balance?bucket={}",
            self.gateway_url.trim_end_matches('/'),
            urlencoding::encode(&bucket),
        );
        let resp = self
            .http
            .get(&url)
            .header("x-machine-auth-secret", self.machine_secret)
            .send()
            .await
            .map_err(|e| crate::application::app_error::AppError::Internal(format!("mesh: {e}")))?;

        if !resp.status().is_success() {
            return Ok(0);
        }
        let body: BalancesResponse = resp.json().await.map_err(|e| {
            crate::application::app_error::AppError::Internal(format!("mesh parse: {e}"))
        })?;

        Ok(body.accounts.first().map(|b| b.balance).unwrap_or(0))
    }

    pub async fn create_billing_event(
        &self,
        user_id: Uuid,
        amount: i64,
        note: &str,
        idempotency_nonce: &str,
    ) -> AppResult<()> {
        let url = format!(
            "{}/internal/billing/events",
            self.gateway_url.trim_end_matches('/')
        );
        let payload = CreateEventPayload {
            bucket: Self::billing_bucket(user_id),
            account: "credits".to_string(),
            amount,
            note: note.to_string(),
            idempotency_nonce: idempotency_nonce.to_string(),
        };

        let resp = self
            .http
            .post(&url)
            .header("x-machine-auth-secret", self.machine_secret)
            .json(&payload)
            .send()
            .await
            .map_err(|e| crate::application::app_error::AppError::Internal(format!("mesh: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(crate::application::app_error::AppError::Internal(format!(
                "mesh event creation failed ({status}): {body}"
            )));
        }

        Ok(())
    }
}
