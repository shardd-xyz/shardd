use async_trait::async_trait;
use sqlx::FromRow;
use uuid::Uuid;

use crate::{
    adapters::persistence::PostgresPersistence,
    application::{
        app_error::AppResult,
        billing::{BillingPlan, BillingRepo, Subscription},
    },
};

// Row types for sqlx::FromRow (avoids compile-time query checking)
#[derive(FromRow)]
struct PlanRow {
    id: Uuid,
    slug: String,
    name: String,
    monthly_credits: i64,
    price_cents: i32,
    annual_price_cents: i32,
    stripe_price_id: Option<String>,
    stripe_annual_price_id: Option<String>,
    is_active: bool,
}

impl From<PlanRow> for BillingPlan {
    fn from(r: PlanRow) -> Self {
        BillingPlan {
            id: r.id,
            slug: r.slug,
            name: r.name,
            monthly_credits: r.monthly_credits,
            price_cents: r.price_cents,
            annual_price_cents: r.annual_price_cents,
            stripe_price_id: r.stripe_price_id,
            stripe_annual_price_id: r.stripe_annual_price_id,
            is_active: r.is_active,
        }
    }
}

#[derive(FromRow)]
struct SubRow {
    user_id: Uuid,
    user_email: String,
    plan_id: Uuid,
    stripe_customer_id: Option<String>,
    stripe_subscription_id: Option<String>,
    subscription_status: String,
    period_start: Option<chrono::DateTime<chrono::Utc>>,
    period_end: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<SubRow> for Subscription {
    fn from(r: SubRow) -> Self {
        Subscription {
            user_id: r.user_id,
            user_email: r.user_email,
            plan_id: r.plan_id,
            stripe_customer_id: r.stripe_customer_id,
            stripe_subscription_id: r.stripe_subscription_id,
            subscription_status: r.subscription_status,
            period_start: r.period_start,
            period_end: r.period_end,
        }
    }
}

#[async_trait]
impl BillingRepo for PostgresPersistence {
    async fn list_active_plans(&self) -> AppResult<Vec<BillingPlan>> {
        let rows: Vec<PlanRow> = sqlx::query_as(
            "SELECT id, slug, name, monthly_credits, price_cents, annual_price_cents, stripe_price_id, stripe_annual_price_id, is_active \
             FROM billing_plans WHERE is_active = true ORDER BY price_cents"
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn get_plan(&self, id: Uuid) -> AppResult<Option<BillingPlan>> {
        let row: Option<PlanRow> = sqlx::query_as(
            "SELECT id, slug, name, monthly_credits, price_cents, annual_price_cents, stripe_price_id, stripe_annual_price_id, is_active \
             FROM billing_plans WHERE id = $1"
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    async fn get_plan_by_slug(&self, slug: &str) -> AppResult<Option<BillingPlan>> {
        let row: Option<PlanRow> = sqlx::query_as(
            "SELECT id, slug, name, monthly_credits, price_cents, annual_price_cents, stripe_price_id, stripe_annual_price_id, is_active \
             FROM billing_plans WHERE slug = $1"
        )
        .bind(slug)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    async fn get_plan_by_stripe_price(
        &self,
        stripe_price_id: &str,
    ) -> AppResult<Option<BillingPlan>> {
        let row: Option<PlanRow> = sqlx::query_as(
            "SELECT id, slug, name, monthly_credits, price_cents, annual_price_cents, stripe_price_id, stripe_annual_price_id, is_active \
             FROM billing_plans WHERE stripe_price_id = $1"
        )
        .bind(stripe_price_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    async fn get_or_create_subscription(
        &self,
        user_id: Uuid,
        email: &str,
    ) -> AppResult<Subscription> {
        let existing: Option<SubRow> = sqlx::query_as(
            "SELECT user_id, user_email, plan_id, stripe_customer_id, stripe_subscription_id, \
                    subscription_status, period_start, period_end \
             FROM subscriptions WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(row) = existing {
            return Ok(row.into());
        }

        let free_plan_id: Uuid =
            sqlx::query_scalar("SELECT id FROM billing_plans WHERE slug = 'free'")
                .fetch_one(&self.pool)
                .await?;

        let email = if email.is_empty() { "unknown" } else { email };
        sqlx::query(
            "INSERT INTO subscriptions (user_id, user_email, plan_id, subscription_status) \
             VALUES ($1, $2, $3, 'free') ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(email)
        .bind(free_plan_id)
        .execute(&self.pool)
        .await?;

        let row: SubRow = sqlx::query_as(
            "SELECT user_id, user_email, plan_id, stripe_customer_id, stripe_subscription_id, \
                    subscription_status, period_start, period_end \
             FROM subscriptions WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_one(&self.pool)
        .await?;

        Ok(row.into())
    }

    async fn get_subscription_by_stripe_customer(
        &self,
        customer_id: &str,
    ) -> AppResult<Option<Subscription>> {
        let row: Option<SubRow> = sqlx::query_as(
            "SELECT user_id, user_email, plan_id, stripe_customer_id, stripe_subscription_id, \
                    subscription_status, period_start, period_end \
             FROM subscriptions WHERE stripe_customer_id = $1",
        )
        .bind(customer_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    async fn get_subscription_by_stripe_subscription(
        &self,
        sub_id: &str,
    ) -> AppResult<Option<Subscription>> {
        let row: Option<SubRow> = sqlx::query_as(
            "SELECT user_id, user_email, plan_id, stripe_customer_id, stripe_subscription_id, \
                    subscription_status, period_start, period_end \
             FROM subscriptions WHERE stripe_subscription_id = $1",
        )
        .bind(sub_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    async fn set_stripe_customer_id(&self, user_id: Uuid, customer_id: &str) -> AppResult<()> {
        sqlx::query("UPDATE subscriptions SET stripe_customer_id = $1 WHERE user_id = $2")
            .bind(customer_id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn activate_subscription(
        &self,
        user_id: Uuid,
        stripe_subscription_id: &str,
        plan_id: Uuid,
        period_start: &Option<chrono::DateTime<chrono::Utc>>,
        period_end: &Option<chrono::DateTime<chrono::Utc>>,
    ) -> AppResult<()> {
        sqlx::query(
            "UPDATE subscriptions SET stripe_subscription_id = $1, plan_id = $2, \
             subscription_status = 'active', period_start = $3, period_end = $4 \
             WHERE user_id = $5",
        )
        .bind(stripe_subscription_id)
        .bind(plan_id)
        .bind(*period_start)
        .bind(*period_end)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_subscription_status(&self, user_id: Uuid, status: &str) -> AppResult<()> {
        sqlx::query("UPDATE subscriptions SET subscription_status = $1 WHERE user_id = $2")
            .bind(status)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn set_plan_manual(
        &self,
        user_id: Uuid,
        plan_id: Uuid,
        user_email: &str,
    ) -> AppResult<()> {
        let email = if user_email.is_empty() {
            "unknown"
        } else {
            user_email
        };
        sqlx::query(
            "INSERT INTO subscriptions (user_id, user_email, plan_id, subscription_status) \
             VALUES ($1, $2, $3, 'manual') \
             ON CONFLICT (user_id) DO UPDATE SET \
                 user_email = EXCLUDED.user_email, \
                 plan_id = EXCLUDED.plan_id, \
                 subscription_status = 'manual', \
                 updated_at = NOW()",
        )
        .bind(user_id)
        .bind(email)
        .bind(plan_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn update_period(
        &self,
        user_id: Uuid,
        period_start: &Option<chrono::DateTime<chrono::Utc>>,
        period_end: &Option<chrono::DateTime<chrono::Utc>>,
    ) -> AppResult<()> {
        sqlx::query(
            "UPDATE subscriptions SET period_start = $1, period_end = $2 WHERE user_id = $3",
        )
        .bind(*period_start)
        .bind(*period_end)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn cancel_subscription(&self, user_id: Uuid, free_plan_id: Uuid) -> AppResult<()> {
        sqlx::query(
            "UPDATE subscriptions SET subscription_status = 'canceled', \
             plan_id = $1, stripe_subscription_id = NULL WHERE user_id = $2",
        )
        .bind(free_plan_id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn is_stripe_event_processed(&self, event_id: &str) -> AppResult<bool> {
        let row: Option<(bool,)> =
            sqlx::query_as("SELECT true FROM processed_stripe_events WHERE stripe_event_id = $1")
                .bind(event_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.is_some())
    }

    async fn mark_stripe_event_processed(&self, event_id: &str, event_type: &str) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO processed_stripe_events (stripe_event_id, event_type) \
             VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(event_id)
        .bind(event_type)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_all_subscriptions(&self) -> AppResult<Vec<Subscription>> {
        let rows: Vec<SubRow> = sqlx::query_as(
            "SELECT user_id, user_email, plan_id, stripe_customer_id, stripe_subscription_id, \
                    subscription_status, period_start, period_end \
             FROM subscriptions",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    async fn was_notification_sent(&self, user_id: Uuid, threshold: &str) -> AppResult<bool> {
        let row: Option<(bool,)> = sqlx::query_as(
            "SELECT true FROM billing_notifications WHERE user_id = $1 AND threshold = $2",
        )
        .bind(user_id)
        .bind(threshold)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    async fn mark_notification_sent(&self, user_id: Uuid, threshold: &str) -> AppResult<()> {
        sqlx::query(
            "INSERT INTO billing_notifications (user_id, threshold) VALUES ($1, $2) ON CONFLICT DO NOTHING"
        )
        .bind(user_id)
        .bind(threshold)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn clear_notifications(&self, user_id: Uuid) -> AppResult<()> {
        sqlx::query("DELETE FROM billing_notifications WHERE user_id = $1")
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
