use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    adapters::http::{app_state::AppState, extractors::MachineAuth},
    application::{
        app_error::{AppError, AppResult},
        billing::MeshClient,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/subscriptions/{user_id}", get(get_subscription))
        .route("/subscriptions/{user_id}/plan", post(set_plan))
        .route("/subscriptions/{user_id}/credits", post(grant_credits))
}

#[derive(Serialize)]
struct AdminSubscriptionDto {
    plan_slug: String,
    plan_name: String,
    monthly_credits: i64,
    credit_balance: i64,
    subscription_status: String,
    period_start: Option<chrono::DateTime<chrono::Utc>>,
    period_end: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Deserialize)]
struct EmailQuery {
    #[serde(default)]
    email: String,
}

#[derive(Deserialize)]
struct SetPlanRequest {
    plan_slug: String,
    #[serde(default)]
    user_email: String,
}

#[derive(Deserialize)]
struct GrantCreditsRequest {
    amount: i64,
    note: String,
}

async fn load_subscription_dto(
    state: &AppState,
    user_id: Uuid,
    email: &str,
) -> AppResult<AdminSubscriptionDto> {
    let sub = state
        .billing_repo
        .get_or_create_subscription(user_id, email)
        .await?;
    let plan = state
        .billing_repo
        .get_plan(sub.plan_id)
        .await?
        .ok_or_else(|| AppError::Internal("plan not found".into()))?;

    let mesh = MeshClient::new(
        &state.config.gateway_url,
        state.config.gateway_machine_auth_secret.expose_secret(),
        &state.http_client,
    );
    let balance = mesh.get_billing_balance(user_id).await.unwrap_or(0);

    Ok(AdminSubscriptionDto {
        plan_slug: plan.slug,
        plan_name: plan.name,
        monthly_credits: plan.monthly_credits,
        credit_balance: balance,
        subscription_status: sub.subscription_status,
        period_start: sub.period_start,
        period_end: sub.period_end,
    })
}

async fn get_subscription(
    _: MachineAuth,
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Query(q): Query<EmailQuery>,
) -> AppResult<Json<AdminSubscriptionDto>> {
    Ok(Json(
        load_subscription_dto(&state, user_id, &q.email).await?,
    ))
}

async fn set_plan(
    _: MachineAuth,
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Json(req): Json<SetPlanRequest>,
) -> AppResult<Json<AdminSubscriptionDto>> {
    let plan = state
        .billing_repo
        .get_plan_by_slug(&req.plan_slug)
        .await?
        .ok_or_else(|| AppError::InvalidInput(format!("unknown plan: {}", req.plan_slug)))?;
    state
        .billing_repo
        .set_plan_manual(user_id, plan.id, &req.user_email)
        .await?;

    // Reset the credit balance to match the plan's monthly allowance.
    // The Enterprise tier has monthly_credits = 0 — that's a "talk to us"
    // marker, not an instruction to zero the user out, so we skip the sync
    // for zero-credit plans and leave the existing balance intact.
    if plan.monthly_credits > 0 {
        let mesh = MeshClient::new(
            &state.config.gateway_url,
            state.config.gateway_machine_auth_secret.expose_secret(),
            &state.http_client,
        );
        let balance = mesh.get_billing_balance(user_id).await.unwrap_or(0);
        let delta = plan.monthly_credits - balance;
        if delta != 0 {
            let nonce = format!("admin_plan_{user_id}_{}", Uuid::new_v4());
            let note = format!("Plan assigned: {} ({delta:+})", plan.name);
            mesh.create_billing_event(user_id, delta, &note, &nonce)
                .await?;
        }
    }

    Ok(Json(
        load_subscription_dto(&state, user_id, &req.user_email).await?,
    ))
}

async fn grant_credits(
    _: MachineAuth,
    State(state): State<AppState>,
    Path(user_id): Path<Uuid>,
    Json(req): Json<GrantCreditsRequest>,
) -> AppResult<StatusCode> {
    if req.amount == 0 {
        return Err(AppError::InvalidInput("amount must be non-zero".into()));
    }
    if req.note.trim().is_empty() {
        return Err(AppError::InvalidInput("note is required".into()));
    }
    let mesh = MeshClient::new(
        &state.config.gateway_url,
        state.config.gateway_machine_auth_secret.expose_secret(),
        &state.http_client,
    );
    let nonce = format!("admin_grant_{user_id}_{}", Uuid::new_v4());
    mesh.create_billing_event(user_id, req.amount, &req.note, &nonce)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
