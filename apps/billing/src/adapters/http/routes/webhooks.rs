use axum::{Router, extract::State, http::HeaderMap, routing::post};
use secrecy::ExposeSecret;
use serde::Deserialize;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    adapters::http::app_state::AppState,
    application::{
        app_error::{AppError, AppResult},
        billing::MeshClient,
    },
};

pub fn router() -> Router<AppState> {
    Router::new().route("/stripe", post(stripe_webhook))
}

// Minimal deserialization structs for webhook payloads
#[derive(Deserialize)]
struct WebhookCheckoutSession {
    client_reference_id: Option<String>,
    subscription: Option<String>,
}

#[derive(Deserialize)]
struct WebhookInvoice {
    id: Option<String>,
    customer: Option<String>,
    subscription: Option<String>,
}

#[derive(Deserialize)]
struct WebhookSubscription {
    id: String,
    status: Option<String>,
    current_period_start: Option<i64>,
    current_period_end: Option<i64>,
    items: Option<WebhookSubItems>,
}

#[derive(Deserialize)]
struct WebhookSubItems {
    data: Vec<WebhookSubItem>,
}

#[derive(Deserialize)]
struct WebhookSubItem {
    price: Option<WebhookPrice>,
}

#[derive(Deserialize)]
struct WebhookPrice {
    id: String,
}

async fn stripe_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> AppResult<&'static str> {
    let sig = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let webhook_secret = state.config.stripe_webhook_secret.expose_secret();
    let event = stripe::Webhook::construct_event(&body, sig, webhook_secret).map_err(|e| {
        warn!("Stripe signature verification failed: {e}");
        AppError::Unauthorized
    })?;

    let event_id = event.id.to_string();
    let event_type = event.type_.to_string();

    if state
        .billing_repo
        .is_stripe_event_processed(&event_id)
        .await?
    {
        info!(event_id, "Stripe event already processed, skipping");
        return Ok("ok");
    }
    state
        .billing_repo
        .mark_stripe_event_processed(&event_id, &event_type)
        .await?;

    let mesh = MeshClient::new(
        &state.config.gateway_url,
        state.config.gateway_machine_auth_secret.expose_secret(),
        &state.http_client,
    );

    // Parse the object as raw JSON to avoid version-specific struct mismatches
    let obj = serde_json::to_value(&event.data.object)
        .map_err(|e| AppError::Internal(format!("serialize: {e}")))?;

    match event.type_ {
        stripe::EventType::CheckoutSessionCompleted => {
            let session: WebhookCheckoutSession = serde_json::from_value(obj)
                .map_err(|e| AppError::Internal(format!("parse checkout: {e}")))?;
            handle_checkout_completed(&state, &mesh, &session, &event_id).await?;
        }
        stripe::EventType::InvoicePaid => {
            let invoice: WebhookInvoice = serde_json::from_value(obj)
                .map_err(|e| AppError::Internal(format!("parse invoice: {e}")))?;
            handle_invoice_paid(&state, &mesh, &invoice).await?;
        }
        stripe::EventType::CustomerSubscriptionUpdated => {
            let sub: WebhookSubscription = serde_json::from_value(obj)
                .map_err(|e| AppError::Internal(format!("parse sub: {e}")))?;
            handle_subscription_updated(&state, &sub).await?;
        }
        stripe::EventType::CustomerSubscriptionDeleted => {
            let sub: WebhookSubscription = serde_json::from_value(obj)
                .map_err(|e| AppError::Internal(format!("parse sub: {e}")))?;
            handle_subscription_deleted(&state, &mesh, &sub, &event_id).await?;
        }
        _ => {
            info!(event_type = %event.type_, "Ignoring unhandled Stripe event type");
        }
    }

    Ok("ok")
}

fn ts_to_dt(ts: i64) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::from_timestamp(ts, 0)
}

async fn handle_checkout_completed(
    state: &AppState,
    mesh: &MeshClient<'_>,
    session: &WebhookCheckoutSession,
    event_id: &str,
) -> AppResult<()> {
    let user_id: Uuid = session
        .client_reference_id
        .as_deref()
        .unwrap_or("")
        .parse()
        .map_err(|_| AppError::Internal("bad user_id in checkout session".into()))?;

    let sub_id = match &session.subscription {
        Some(id) => id.clone(),
        None => return Ok(()),
    };

    // Fetch subscription from Stripe to get price/period info
    let stripe_sub: WebhookSubscription = {
        let sub_id_parsed: stripe::SubscriptionId = sub_id
            .parse()
            .map_err(|_| AppError::Internal("bad subscription id".into()))?;
        let fetched = stripe::Subscription::retrieve(&state.stripe_client, &sub_id_parsed, &[])
            .await
            .map_err(|e| AppError::Internal(format!("stripe fetch sub: {e}")))?;
        serde_json::from_value(serde_json::to_value(fetched).unwrap_or_default())
            .map_err(|e| AppError::Internal(format!("parse fetched sub: {e}")))?
    };

    let price_id = stripe_sub
        .items
        .as_ref()
        .and_then(|items| items.data.first())
        .and_then(|item| item.price.as_ref())
        .map(|p| p.id.clone());

    if let Some(price_id) = price_id
        && let Some(plan) = state
            .billing_repo
            .get_plan_by_stripe_price(&price_id)
            .await?
    {
        let period_start = stripe_sub.current_period_start.and_then(ts_to_dt);
        let period_end = stripe_sub.current_period_end.and_then(ts_to_dt);

        state
            .billing_repo
            .activate_subscription(user_id, &sub_id, plan.id, &period_start, &period_end)
            .await?;

        // Top up credits
        let balance = mesh.get_billing_balance(user_id).await.unwrap_or(0);
        let delta = (plan.monthly_credits - balance).max(0);
        if delta > 0 {
            mesh.create_billing_event(
                user_id,
                delta,
                &format!("Subscription activated: {} plan", plan.name),
                &format!("stripe:checkout:{event_id}"),
            )
            .await?;
        }

        info!(user_id = %user_id, plan = %plan.slug, "Subscription activated");
    }

    Ok(())
}

async fn handle_invoice_paid(
    state: &AppState,
    mesh: &MeshClient<'_>,
    invoice: &WebhookInvoice,
) -> AppResult<()> {
    let customer_id = match &invoice.customer {
        Some(c) => c.clone(),
        None => return Ok(()),
    };

    let sub = match state
        .billing_repo
        .get_subscription_by_stripe_customer(&customer_id)
        .await?
    {
        Some(s) => s,
        None => {
            warn!(customer_id, "No subscription found for Stripe customer");
            return Ok(());
        }
    };

    let plan = state
        .billing_repo
        .get_plan(sub.plan_id)
        .await?
        .ok_or_else(|| AppError::Internal("plan not found".into()))?;

    let balance = mesh.get_billing_balance(sub.user_id).await.unwrap_or(0);
    let delta = (plan.monthly_credits - balance).max(0);
    if delta > 0 {
        let invoice_id = invoice.id.as_deref().unwrap_or("unknown");
        mesh.create_billing_event(
            sub.user_id,
            delta,
            &format!("Monthly credit: {} plan (invoice {invoice_id})", plan.name),
            &format!("stripe:invoice:{invoice_id}"),
        )
        .await?;
        info!(user_id = %sub.user_id, delta, "Monthly credits topped up");
    }

    // Update period from Stripe subscription if available
    if let Some(stripe_sub_id) = &invoice.subscription
        && let Ok(sub_id) = stripe_sub_id.parse::<stripe::SubscriptionId>()
        && let Ok(fetched) =
            stripe::Subscription::retrieve(&state.stripe_client, &sub_id, &[]).await
    {
        let fetched_json: WebhookSubscription = serde_json::from_value(
            serde_json::to_value(fetched).unwrap_or_default(),
        )
        .unwrap_or(WebhookSubscription {
            id: String::new(),
            status: None,
            current_period_start: None,
            current_period_end: None,
            items: None,
        });
        state
            .billing_repo
            .update_period(
                sub.user_id,
                &fetched_json.current_period_start.and_then(ts_to_dt),
                &fetched_json.current_period_end.and_then(ts_to_dt),
            )
            .await?;
    }

    Ok(())
}

async fn handle_subscription_updated(state: &AppState, sub: &WebhookSubscription) -> AppResult<()> {
    if let Some(existing) = state
        .billing_repo
        .get_subscription_by_stripe_subscription(&sub.id)
        .await?
    {
        let status = sub.status.as_deref().unwrap_or("unknown");
        state
            .billing_repo
            .update_subscription_status(existing.user_id, status)
            .await?;
        info!(user_id = %existing.user_id, status, "Subscription status updated");
    }
    Ok(())
}

async fn handle_subscription_deleted(
    state: &AppState,
    mesh: &MeshClient<'_>,
    sub: &WebhookSubscription,
    event_id: &str,
) -> AppResult<()> {
    if let Some(existing) = state
        .billing_repo
        .get_subscription_by_stripe_subscription(&sub.id)
        .await?
    {
        let free_plan = state
            .billing_repo
            .get_plan_by_slug("free")
            .await?
            .ok_or_else(|| AppError::Internal("free plan missing".into()))?;

        state
            .billing_repo
            .cancel_subscription(existing.user_id, free_plan.id)
            .await?;

        let balance = mesh
            .get_billing_balance(existing.user_id)
            .await
            .unwrap_or(0);
        let delta = (free_plan.monthly_credits - balance).max(0);
        if delta > 0 {
            mesh.create_billing_event(
                existing.user_id,
                delta,
                "Downgraded to Free plan",
                &format!("stripe:cancel:{event_id}"),
            )
            .await?;
        }

        info!(user_id = %existing.user_id, "Subscription cancelled, downgraded to free");
    }
    Ok(())
}
