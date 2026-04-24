use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
};
use axum_extra::extract::cookie::{Cookie, SameSite};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    adapters::http::{app_state::AppState, extractors::AdminUser},
    app_error::{AppError, AppResult},
    application::jwt,
    use_cases::{
        audit::NewAuditEntry,
        user::{UserProfile, UserStats},
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route("/stats", get(stats))
        .route("/users", get(list_users))
        .route("/users/{id}", get(get_user))
        .route("/users/{id}", delete(delete_user))
        .route("/users/{id}/freeze", post(freeze_user))
        .route("/users/{id}/unfreeze", post(unfreeze_user))
        .route("/users/{id}/impersonate", post(impersonate_user))
        .route("/users/{id}/subscription", get(get_user_subscription))
        .route("/users/{id}/subscription/plan", post(set_user_plan))
        .route("/users/{id}/subscription/credits", post(grant_user_credits))
        .route("/billing/plans", get(list_billing_plans))
        .route("/mesh/nodes", get(list_mesh_nodes))
        .route("/buckets/{bucket}", delete(admin_purge_bucket))
        .route("/events", get(list_admin_events))
        .route("/audit", get(list_audit))
}

// ---------- DTOs ----------

#[derive(Serialize)]
struct UserDto {
    id: Uuid,
    email: String,
    language: String,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
    last_login_at: Option<chrono::DateTime<chrono::Utc>>,
    is_admin: bool,
    is_frozen: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<UserProfile> for UserDto {
    fn from(p: UserProfile) -> Self {
        UserDto {
            id: p.id,
            email: p.email,
            language: p.language,
            created_at: p.created_at,
            updated_at: p.updated_at,
            last_login_at: p.last_login_at,
            is_admin: p.is_admin,
            is_frozen: p.is_frozen,
            deleted_at: p.deleted_at,
        }
    }
}

#[derive(Serialize)]
struct StatsDto {
    total_users: i64,
    users_last_7_days: i64,
    users_last_30_days: i64,
    frozen_users: i64,
    admin_users: i64,
}

impl From<UserStats> for StatsDto {
    fn from(s: UserStats) -> Self {
        StatsDto {
            total_users: s.total_users,
            users_last_7_days: s.users_last_7_days,
            users_last_30_days: s.users_last_30_days,
            frozen_users: s.frozen_users,
            admin_users: s.admin_users,
        }
    }
}

#[derive(Deserialize)]
struct ListUsersQuery {
    q: Option<String>,
    #[serde(default)]
    status: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Serialize)]
struct ListUsersDto {
    users: Vec<UserDto>,
    total: i64,
    limit: i64,
    offset: i64,
}

#[derive(Deserialize)]
struct AuditQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

// ---------- Handlers ----------

async fn me(AdminUser(admin): AdminUser) -> AppResult<Json<UserDto>> {
    Ok(Json(admin.into()))
}

async fn stats(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
) -> AppResult<Json<StatsDto>> {
    let stats = state.user_repo.stats().await?;
    Ok(Json(stats.into()))
}

async fn list_users(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Query(q): Query<ListUsersQuery>,
) -> AppResult<Json<ListUsersDto>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);
    let query = q.q.as_deref().filter(|s| !s.is_empty());
    let status = crate::use_cases::user::UserStatusFilter::from_raw(q.status.as_deref());
    let users = state
        .user_repo
        .list_users(query, status, limit, offset)
        .await?;
    let total = state.user_repo.count_users(query, status).await?;
    Ok(Json(ListUsersDto {
        users: users.into_iter().map(Into::into).collect(),
        total,
        limit,
        offset,
    }))
}

async fn get_user(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<UserDto>> {
    // Admin must be able to view soft-deleted accounts so the detail page
    // can surface the `deleted_at` timestamp and hide mutation actions.
    let user = state
        .user_repo
        .get_profile_by_id_any(id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(user.into()))
}

async fn freeze_user(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<UserDto>> {
    let target = state
        .user_repo
        .get_profile_by_id(id)
        .await?
        .ok_or(AppError::NotFound)?;
    if target.id == admin.id {
        return Err(AppError::Conflict("cannot freeze yourself".into()));
    }
    if target.is_admin {
        return Err(AppError::Conflict("cannot freeze another admin".into()));
    }
    state.user_repo.set_frozen(id, true).await?;
    write_audit(&state, &admin, "user.freeze", Some(&target), json!({})).await?;
    let updated = state
        .user_repo
        .get_profile_by_id(id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated.into()))
}

async fn unfreeze_user(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<UserDto>> {
    let target = state
        .user_repo
        .get_profile_by_id(id)
        .await?
        .ok_or(AppError::NotFound)?;
    state.user_repo.set_frozen(id, false).await?;
    write_audit(&state, &admin, "user.unfreeze", Some(&target), json!({})).await?;
    let updated = state
        .user_repo
        .get_profile_by_id(id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(updated.into()))
}

async fn delete_user(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<StatusCode> {
    let target = state
        .user_repo
        .get_profile_by_id(id)
        .await?
        .ok_or(AppError::NotFound)?;
    if target.id == admin.id {
        return Err(AppError::Conflict("cannot delete yourself".into()));
    }
    if target.is_admin {
        return Err(AppError::Conflict("cannot delete another admin".into()));
    }
    // Log BEFORE delete so admin_audit_log.target_user_id still resolves;
    // the ON DELETE SET NULL keeps the trail readable either way.
    write_audit(&state, &admin, "user.delete", Some(&target), json!({})).await?;
    state.user_repo.soft_delete_user(id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn impersonate_user(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<impl IntoResponse> {
    let target = state
        .user_repo
        .get_profile_by_id(id)
        .await?
        .ok_or(AppError::NotFound)?;
    if target.is_admin {
        return Err(AppError::Conflict(
            "cannot impersonate another admin".into(),
        ));
    }
    if target.is_frozen {
        return Err(AppError::Conflict(
            "cannot impersonate a frozen user".into(),
        ));
    }

    let ttl = time::Duration::minutes(state.config.impersonation_ttl_minutes);
    let access = jwt::issue(target.id, &state.config.jwt_secret, ttl)?;

    write_audit(
        &state,
        &admin,
        "user.impersonate",
        Some(&target),
        json!({ "ttl_minutes": state.config.impersonation_ttl_minutes }),
    )
    .await?;

    // Replace the admin's session cookies with impersonation cookies.
    // Admin must log out + log back in to end impersonation.
    // A visible `impersonating` cookie (non-httponly) lets the UI render a banner.
    let mut headers = HeaderMap::new();
    let access_cookie = Cookie::build(("access_token", access))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(ttl)
        .build();
    // Clear refresh so impersonation cannot be extended silently.
    let refresh_cookie = Cookie::build(("refresh_token", ""))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(time::Duration::seconds(0))
        .build();
    let email_cookie = Cookie::build(("user_email", target.email.clone()))
        .http_only(false)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(ttl)
        .build();
    let banner_cookie = Cookie::build(("impersonating", target.email.clone()))
        .http_only(false)
        .same_site(SameSite::Lax)
        .path("/")
        .max_age(ttl)
        .build();
    for c in [access_cookie, refresh_cookie, email_cookie, banner_cookie] {
        headers.append("set-cookie", c.to_string().parse().unwrap());
    }

    Ok((StatusCode::OK, headers, Json(json!({ "ok": true }))))
}

#[derive(Serialize)]
struct AuditListDto {
    entries: Vec<crate::use_cases::audit::AuditEntry>,
    total: i64,
    limit: i64,
    offset: i64,
}

async fn list_audit(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Query(q): Query<AuditQuery>,
) -> AppResult<Json<AuditListDto>> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let offset = q.offset.unwrap_or(0).max(0);
    let entries = state.audit_repo.list(limit, offset).await?;
    let total = state.audit_repo.count().await?;
    Ok(Json(AuditListDto {
        entries,
        total,
        limit,
        offset,
    }))
}

// ---------- Billing proxy (admin) ----------

#[derive(Serialize, Deserialize)]
struct AdminSubscriptionDto {
    plan_slug: String,
    plan_name: String,
    monthly_credits: i64,
    credit_balance: i64,
    subscription_status: String,
    period_start: Option<chrono::DateTime<chrono::Utc>>,
    period_end: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Serialize, Deserialize)]
struct BillingPlanDto {
    slug: String,
    name: String,
    monthly_credits: i64,
    price_cents: i32,
    annual_price_cents: i32,
}

#[derive(Deserialize)]
struct SetPlanBody {
    plan_slug: String,
}

#[derive(Deserialize)]
struct GrantCreditsBody {
    amount: i64,
    note: String,
}

fn billing_bearer(state: &AppState) -> String {
    format!(
        "Bearer {}",
        state.config.billing_internal_secret.expose_secret()
    )
}

async fn billing_get<T: serde::de::DeserializeOwned>(state: &AppState, path: &str) -> AppResult<T> {
    let url = format!(
        "{}{}",
        state.config.billing_base_url.trim_end_matches('/'),
        path,
    );
    let resp = state
        .edge_http
        .get(&url)
        .header(axum::http::header::AUTHORIZATION, billing_bearer(state))
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("billing: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Internal(format!(
            "billing {} failed: {}",
            path,
            resp.status()
        )));
    }
    resp.json::<T>()
        .await
        .map_err(|e| AppError::Internal(format!("billing parse: {e}")))
}

async fn billing_post<B: serde::Serialize, T: serde::de::DeserializeOwned>(
    state: &AppState,
    path: &str,
    body: &B,
) -> AppResult<T> {
    let url = format!(
        "{}{}",
        state.config.billing_base_url.trim_end_matches('/'),
        path,
    );
    let resp = state
        .edge_http
        .post(&url)
        .header(axum::http::header::AUTHORIZATION, billing_bearer(state))
        .json(body)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("billing: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Internal(format!(
            "billing {path} failed ({status}): {body}"
        )));
    }
    resp.json::<T>()
        .await
        .map_err(|e| AppError::Internal(format!("billing parse: {e}")))
}

async fn billing_post_no_body<B: serde::Serialize>(
    state: &AppState,
    path: &str,
    body: &B,
) -> AppResult<()> {
    let url = format!(
        "{}{}",
        state.config.billing_base_url.trim_end_matches('/'),
        path,
    );
    let resp = state
        .edge_http
        .post(&url)
        .header(axum::http::header::AUTHORIZATION, billing_bearer(state))
        .json(body)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("billing: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Internal(format!(
            "billing {path} failed ({status}): {body}"
        )));
    }
    Ok(())
}

async fn get_user_subscription(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<AdminSubscriptionDto>> {
    let target = state
        .user_repo
        .get_profile_by_id(id)
        .await?
        .ok_or(AppError::NotFound)?;
    let path = format!(
        "/internal/admin/subscriptions/{id}?email={}",
        urlencoding::encode(&target.email)
    );
    let dto: AdminSubscriptionDto = billing_get(&state, &path).await?;
    Ok(Json(dto))
}

#[derive(Serialize)]
struct MeshEdgeNodesDto {
    edge_id: String,
    region: String,
    base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    nodes: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Fan out to every configured edge gateway's /internal/mesh/nodes and
/// aggregate into a per-edge list. Lets the admin UI show each gateway's
/// view of the mesh (including every advertised multiaddr per node).
async fn list_mesh_nodes(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<MeshEdgeNodesDto>>> {
    let Some(secret) = state.config.machine_auth_shared_secret.as_ref() else {
        return Err(AppError::Internal("machine auth is not configured".into()));
    };
    // node_id → human label lookup, built from every public_edges.node_label
    // that the cluster state has declared. Lets the UI show e.g.
    // "aws-use1-21c8-mesh" beside the UUID.
    let mut node_labels: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for edge in &state.config.public_edges {
        if let (Some(nid), Some(lbl)) = (&edge.node_id, &edge.node_label) {
            node_labels.insert(nid.clone(), lbl.clone());
        }
    }
    let mut results = Vec::new();
    for edge in &state.config.public_edges {
        let url = format!(
            "{}/internal/mesh/nodes",
            edge.base_url.trim_end_matches('/')
        );
        let resp = state
            .edge_http
            .get(&url)
            .header("x-machine-auth-secret", secret.expose_secret())
            .send()
            .await;
        let (mut nodes, error): (Vec<serde_json::Value>, _) = match resp {
            Ok(r) if r.status().is_success() => match r.json::<Vec<serde_json::Value>>().await {
                Ok(list) => (list, None),
                Err(e) => (Vec::new(), Some(format!("parse: {e}"))),
            },
            Ok(r) => (Vec::new(), Some(format!("status {}", r.status()))),
            Err(e) => (Vec::new(), Some(e.to_string())),
        };
        for node in nodes.iter_mut() {
            let nid = node
                .get("node_id")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            if let (Some(nid), Some(obj)) = (nid, node.as_object_mut())
                && let Some(lbl) = node_labels.get(&nid)
            {
                obj.insert("label".to_string(), serde_json::Value::String(lbl.clone()));
            }
        }
        results.push(MeshEdgeNodesDto {
            edge_id: edge.edge_id.clone(),
            region: edge.region.clone(),
            base_url: edge.base_url.clone(),
            label: edge.label.clone(),
            nodes,
            error,
        });
    }
    Ok(Json(results))
}

async fn list_billing_plans(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<BillingPlanDto>>> {
    // The public plans route needs no bearer, but we can reuse the same client.
    let url = format!(
        "{}/api/billing/plans",
        state.config.billing_base_url.trim_end_matches('/'),
    );
    let resp = state
        .edge_http
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("billing: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Internal(format!(
            "billing plans failed: {}",
            resp.status()
        )));
    }
    let plans: Vec<BillingPlanDto> = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("billing parse: {e}")))?;
    Ok(Json(plans))
}

async fn set_user_plan(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<SetPlanBody>,
) -> AppResult<Json<AdminSubscriptionDto>> {
    let target = state
        .user_repo
        .get_profile_by_id(id)
        .await?
        .ok_or(AppError::NotFound)?;

    #[derive(Serialize)]
    struct BillingSetPlan<'a> {
        plan_slug: &'a str,
        user_email: &'a str,
    }
    let body = BillingSetPlan {
        plan_slug: &req.plan_slug,
        user_email: &target.email,
    };
    let path = format!("/internal/admin/subscriptions/{id}/plan");
    let dto: AdminSubscriptionDto = billing_post(&state, &path, &body).await?;

    write_audit(
        &state,
        &admin,
        "user.plan_assign",
        Some(&target),
        json!({ "plan_slug": req.plan_slug }),
    )
    .await?;

    Ok(Json(dto))
}

async fn grant_user_credits(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<GrantCreditsBody>,
) -> AppResult<StatusCode> {
    if req.amount == 0 {
        return Err(AppError::InvalidInput("amount must be non-zero".into()));
    }
    if req.note.trim().is_empty() {
        return Err(AppError::InvalidInput("note is required".into()));
    }
    let target = state
        .user_repo
        .get_profile_by_id(id)
        .await?
        .ok_or(AppError::NotFound)?;

    #[derive(Serialize)]
    struct BillingGrant<'a> {
        amount: i64,
        note: &'a str,
    }
    let body = BillingGrant {
        amount: req.amount,
        note: &req.note,
    };
    let path = format!("/internal/admin/subscriptions/{id}/credits");
    billing_post_no_body(&state, &path, &body).await?;

    write_audit(
        &state,
        &admin,
        "user.credits_grant",
        Some(&target),
        json!({ "amount": req.amount, "note": req.note }),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// §3.5: admin-triggered hard-delete of any bucket (including ones the
/// admin doesn't own). Mirrors `developer_buckets::purge_bucket` but
/// skips the ownership check. Logs to the admin audit table.
async fn admin_purge_bucket(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(bucket): Path<String>,
) -> AppResult<StatusCode> {
    if bucket.is_empty() {
        return Err(AppError::InvalidInput("bucket is required".into()));
    }

    let Some(secret) = state.config.machine_auth_shared_secret.as_ref() else {
        return Err(AppError::Internal("machine auth is not configured".into()));
    };

    // Try every configured edge until one accepts. Meta events propagate
    // via gossip, so any edge that can talk to its local node is fine.
    let mut last_err: Option<String> = None;
    let mut ok = false;
    for edge in &state.config.public_edges {
        let url = format!(
            "{}/internal/meta/bucket-delete",
            edge.base_url.trim_end_matches('/')
        );
        let resp = state
            .edge_http
            .post(&url)
            .header("x-machine-auth-secret", secret.expose_secret())
            .json(&json!({
                "bucket": bucket,
                "reason": format!("admin:{}", admin.email),
            }))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                ok = true;
                break;
            }
            Ok(r) => {
                last_err = Some(format!("edge {} returned {}", edge.edge_id, r.status()));
            }
            Err(e) => {
                last_err = Some(format!("edge {}: {}", edge.edge_id, e));
            }
        }
    }
    if !ok {
        return Err(AppError::Internal(
            last_err.unwrap_or_else(|| "no edge accepted bucket-delete".into()),
        ));
    }

    write_audit(
        &state,
        &admin,
        "bucket_purged",
        None,
        json!({ "bucket": bucket }),
    )
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Admin events viewer. Cluster-wide event listing with offset
/// pagination + filters. Proxies to the gateway's machine-auth
/// `/internal/admin/events`. The `?replication=true` bit triggers a
/// per-edge heads fan-out on the gateway side so the UI's detail
/// drawer can render a replication matrix in one round-trip.
#[derive(Debug, Deserialize)]
struct AdminEventsQuery {
    #[serde(default)]
    bucket: Option<String>,
    #[serde(default)]
    account: Option<String>,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    event_type: Option<String>,
    #[serde(default)]
    since_ms: Option<u64>,
    #[serde(default)]
    until_ms: Option<u64>,
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    offset: Option<i64>,
    #[serde(default)]
    replication: Option<bool>,
}

async fn list_admin_events(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Query(q): Query<AdminEventsQuery>,
) -> AppResult<axum::response::Response> {
    // Dashboard-side limit cap: 200 rows per page. The node clamps at 500;
    // this tighter cap keeps admin pages snappy.
    let limit = q.limit.unwrap_or(100).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);
    let limit_s = limit.to_string();
    let offset_s = offset.to_string();
    let since_s = q.since_ms.map(|v| v.to_string());
    let until_s = q.until_ms.map(|v| v.to_string());
    let replication_s = match q.replication {
        Some(true) => Some("true".to_string()),
        _ => None,
    };
    let path = crate::adapters::http::routes::developer_buckets::path_with_query(
        "/internal/admin/events",
        &[
            ("bucket", q.bucket.as_deref()),
            ("account", q.account.as_deref()),
            ("origin", q.origin.as_deref()),
            ("event_type", q.event_type.as_deref()),
            ("since_ms", since_s.as_deref()),
            ("until_ms", until_s.as_deref()),
            ("search", q.search.as_deref()),
            ("limit", Some(&limit_s)),
            ("offset", Some(&offset_s)),
            ("replication", replication_s.as_deref()),
        ],
    );
    crate::adapters::http::routes::developer_buckets::proxy_gateway_json(
        &state,
        reqwest::Method::GET,
        path,
        None,
    )
    .await
}

// ---------- helpers ----------

async fn write_audit(
    state: &AppState,
    admin: &UserProfile,
    action: &str,
    target: Option<&UserProfile>,
    metadata: serde_json::Value,
) -> AppResult<()> {
    state
        .audit_repo
        .log(NewAuditEntry {
            admin_id: admin.id,
            admin_email: admin.email.clone(),
            action: action.to_string(),
            target_user_id: target.map(|t| t.id),
            target_email: target.map(|t| t.email.clone()),
            metadata,
        })
        .await
}
