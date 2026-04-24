use crate::api::billing::BillingPlan;
use crate::api::{ApiError, api_get, api_post, api_post_no_body, api_post_with_body};
use crate::types::*;
use serde::Serialize;

pub async fn stats() -> Result<AdminStats, ApiError> {
    api_get("/api/admin/stats").await
}

pub async fn list_users(
    query: &str,
    status: &str,
    page: usize,
    limit: usize,
) -> Result<UserListResponse, ApiError> {
    let offset = (page - 1) * limit;
    let mut params = vec![format!("limit={limit}"), format!("offset={offset}")];
    if !query.is_empty() {
        params.push(format!("q={}", urlencoding::encode(query)));
    }
    // status ∈ {"active","deleted","all"}; empty defaults server-side to active.
    if !status.is_empty() && status != "active" {
        params.push(format!("status={}", status));
    }
    api_get(&format!("/api/admin/users?{}", params.join("&"))).await
}

pub async fn get_user(user_id: &str) -> Result<UserSummary, ApiError> {
    api_get(&format!("/api/admin/users/{user_id}")).await
}

#[allow(dead_code)]
pub async fn freeze_user(user_id: &str) -> Result<(), ApiError> {
    api_post_no_body(&format!("/api/admin/users/{user_id}/freeze")).await
}

#[allow(dead_code)]
pub async fn unfreeze_user(user_id: &str) -> Result<(), ApiError> {
    api_post_no_body(&format!("/api/admin/users/{user_id}/unfreeze")).await
}

#[allow(dead_code)]
pub async fn impersonate_user(user_id: &str) -> Result<(), ApiError> {
    api_post_no_body(&format!("/api/admin/users/{user_id}/impersonate")).await
}

pub async fn list_audit(page: usize, limit: usize) -> Result<AuditListResponse, ApiError> {
    let offset = (page - 1) * limit;
    api_get(&format!("/api/admin/audit?limit={limit}&offset={offset}")).await
}

pub async fn get_subscription(user_id: &str) -> Result<AdminSubscription, ApiError> {
    api_get(&format!("/api/admin/users/{user_id}/subscription")).await
}

pub async fn list_plans() -> Result<Vec<BillingPlan>, ApiError> {
    api_get("/api/admin/billing/plans").await
}

#[derive(Serialize)]
struct SetPlanBody<'a> {
    plan_slug: &'a str,
}

pub async fn set_plan(user_id: &str, plan_slug: &str) -> Result<AdminSubscription, ApiError> {
    api_post(
        &format!("/api/admin/users/{user_id}/subscription/plan"),
        &SetPlanBody { plan_slug },
    )
    .await
}

#[derive(Serialize)]
struct GrantCreditsBody<'a> {
    amount: i64,
    note: &'a str,
}

pub async fn grant_credits(user_id: &str, amount: i64, note: &str) -> Result<(), ApiError> {
    api_post_with_body(
        &format!("/api/admin/users/{user_id}/subscription/credits"),
        &GrantCreditsBody { amount, note },
    )
    .await
}

pub async fn list_mesh_nodes() -> Result<Vec<MeshEdgeNodes>, ApiError> {
    api_get("/api/admin/mesh/nodes").await
}

pub async fn list_events(
    filter: &AdminEventsFilter,
    page: usize,
    limit: usize,
    replication: bool,
) -> Result<AdminEventListResponse, ApiError> {
    let offset = (page - 1) * limit;
    let mut params = vec![format!("limit={limit}"), format!("offset={offset}")];
    if !filter.bucket.is_empty() {
        params.push(format!("bucket={}", urlencoding::encode(&filter.bucket)));
    }
    if !filter.account.is_empty() {
        params.push(format!("account={}", urlencoding::encode(&filter.account)));
    }
    if !filter.origin.is_empty() {
        params.push(format!("origin={}", urlencoding::encode(&filter.origin)));
    }
    if !filter.event_type.is_empty() {
        params.push(format!(
            "event_type={}",
            urlencoding::encode(&filter.event_type)
        ));
    }
    if let Some(since) = filter.since_ms {
        params.push(format!("since_ms={since}"));
    }
    if let Some(until) = filter.until_ms {
        params.push(format!("until_ms={until}"));
    }
    if !filter.search.is_empty() {
        params.push(format!("search={}", urlencoding::encode(&filter.search)));
    }
    if replication {
        params.push("replication=true".to_string());
    }
    api_get(&format!("/api/admin/events?{}", params.join("&"))).await
}
