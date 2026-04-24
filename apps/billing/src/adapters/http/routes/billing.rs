use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};

use crate::{
    adapters::http::{app_state::AppState, extractors::AuthUser},
    application::{
        app_error::{AppError, AppResult},
        billing::MeshClient,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/status", get(status))
        .route("/plans", get(plans))
        .route("/checkout", post(checkout))
        .route("/portal", post(portal))
}

#[derive(Serialize)]
struct BillingStatusDto {
    plan_name: String,
    plan_slug: String,
    monthly_credits: i64,
    credit_balance: i64,
    subscription_status: String,
    period_start: Option<chrono::DateTime<chrono::Utc>>,
    period_end: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize)]
struct PlanDto {
    slug: String,
    name: String,
    monthly_credits: i64,
    price_cents: i32,
    annual_price_cents: i32,
}

#[derive(Deserialize)]
struct CheckoutRequest {
    plan_slug: String,
    #[serde(default)]
    annual: bool,
}

#[derive(Serialize)]
struct UrlResponse {
    url: String,
}

async fn status(
    AuthUser(user_id): AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<BillingStatusDto>> {
    let sub = state
        .billing_repo
        .get_or_create_subscription(user_id, "")
        .await?;
    let plan = state
        .billing_repo
        .get_plan(sub.plan_id)
        .await?
        .ok_or(AppError::Internal("plan not found".into()))?;

    let mesh = MeshClient::new(
        &state.config.gateway_url,
        state.config.gateway_machine_auth_secret.expose_secret(),
        &state.http_client,
    );
    let balance = mesh.get_billing_balance(user_id).await.unwrap_or(0);

    // Auto-provision: top up to plan allowance if below
    let balance = if balance < plan.monthly_credits && plan.monthly_credits > 0 {
        let delta = plan.monthly_credits - balance;
        if mesh
            .create_billing_event(
                user_id,
                delta,
                &format!("Credit grant: {} plan", plan.name),
                &format!("grant_{user_id}_{}", chrono::Utc::now().format("%Y%m")),
            )
            .await
            .is_ok()
        {
            plan.monthly_credits
        } else {
            balance
        }
    } else {
        balance
    };

    Ok(Json(BillingStatusDto {
        plan_name: plan.name,
        plan_slug: plan.slug,
        monthly_credits: plan.monthly_credits,
        credit_balance: balance,
        subscription_status: sub.subscription_status,
        period_start: sub.period_start,
        period_end: sub.period_end,
    }))
}

async fn plans(State(state): State<AppState>) -> AppResult<Json<Vec<PlanDto>>> {
    let plans = state.billing_repo.list_active_plans().await?;
    Ok(Json(
        plans
            .into_iter()
            .map(|p| PlanDto {
                slug: p.slug,
                name: p.name,
                monthly_credits: p.monthly_credits,
                price_cents: p.price_cents,
                annual_price_cents: p.annual_price_cents,
            })
            .collect(),
    ))
}

async fn checkout(
    AuthUser(user_id): AuthUser,
    State(state): State<AppState>,
    Json(req): Json<CheckoutRequest>,
) -> AppResult<Json<UrlResponse>> {
    let plan = state
        .billing_repo
        .get_plan_by_slug(&req.plan_slug)
        .await?
        .ok_or(AppError::NotFound)?;
    let stripe_price_id = if req.annual {
        plan.stripe_annual_price_id.ok_or_else(|| {
            AppError::InvalidInput("annual billing not available for this plan".into())
        })?
    } else {
        plan.stripe_price_id
            .ok_or_else(|| AppError::InvalidInput("free plan cannot be checked out".into()))?
    };

    let sub = state
        .billing_repo
        .get_or_create_subscription(user_id, "")
        .await?;

    let customer_id = if let Some(cid) = &sub.stripe_customer_id {
        cid.parse()
            .map_err(|_| AppError::Internal("bad customer id".into()))?
    } else {
        let customer = stripe::Customer::create(
            &state.stripe_client,
            stripe::CreateCustomer {
                metadata: Some(
                    [("user_id".into(), user_id.to_string())]
                        .into_iter()
                        .collect(),
                ),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| AppError::Internal(format!("stripe: {e}")))?;
        state
            .billing_repo
            .set_stripe_customer_id(user_id, customer.id.as_ref())
            .await?;
        customer.id
    };

    // Strip a trailing slash on app_origin so we don't produce
    // `https://app.shardd.xyz//dashboard/billing` — Stripe renders that into
    // the return button and it 404s on click. `Url::as_str()` always emits a
    // trailing slash for root-only URLs.
    let origin = state.config.app_origin.as_str().trim_end_matches('/');
    let success_url = format!(
        "{}/dashboard/billing?session_id={{CHECKOUT_SESSION_ID}}",
        origin
    );
    let cancel_url = format!("{}/dashboard/billing", origin);
    let ref_id = user_id.to_string();

    let mut params = stripe::CreateCheckoutSession::new();
    params.customer = Some(customer_id);
    params.mode = Some(stripe::CheckoutSessionMode::Subscription);
    params.line_items = Some(vec![stripe::CreateCheckoutSessionLineItems {
        price: Some(stripe_price_id),
        quantity: Some(1),
        ..Default::default()
    }]);
    params.success_url = Some(&success_url);
    params.cancel_url = Some(&cancel_url);
    params.client_reference_id = Some(&ref_id);

    let session = stripe::CheckoutSession::create(&state.stripe_client, params)
        .await
        .map_err(|e| AppError::Internal(format!("stripe: {e}")))?;

    let url = session
        .url
        .ok_or(AppError::Internal("no checkout url".into()))?;
    Ok(Json(UrlResponse { url }))
}

async fn portal(
    AuthUser(user_id): AuthUser,
    State(state): State<AppState>,
) -> AppResult<Json<UrlResponse>> {
    let sub = state
        .billing_repo
        .get_or_create_subscription(user_id, "")
        .await?;
    let customer_id: stripe::CustomerId = sub
        .stripe_customer_id
        .ok_or_else(|| AppError::InvalidInput("no active subscription".into()))?
        .parse()
        .map_err(|_| AppError::Internal("bad customer id".into()))?;

    let return_url = format!(
        "{}/dashboard/billing",
        state.config.app_origin.as_str().trim_end_matches('/')
    );
    let mut params = stripe::CreateBillingPortalSession::new(customer_id);
    params.return_url = Some(&return_url);

    let session = stripe::BillingPortalSession::create(&state.stripe_client, params)
        .await
        .map_err(|e| AppError::Internal(format!("stripe: {e}")))?;

    Ok(Json(UrlResponse { url: session.url }))
}
