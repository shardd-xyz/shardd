use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, post},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::{
    adapters::http::{app_state::AppState, extractors::AdminUser},
    app_error::{AppError, AppResult},
    use_cases::developer_auth::{
        DeveloperAccount, DeveloperApiKey, DeveloperApiKeyScope, DeveloperAuthAuditEntry,
        DeveloperAuthRepo, NewDeveloperAuthAuditEntry, ScopeMatchType, ScopeResourceType,
    },
    use_cases::user::UserProfile,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/users/{id}/developer", get(get_user_developer))
        .route(
            "/users/{id}/developer/keys",
            get(list_user_keys).post(create_user_key),
        )
        .route(
            "/users/{user_id}/developer/keys/{key_id}/revoke",
            post(revoke_user_key),
        )
        .route(
            "/users/{user_id}/developer/keys/{key_id}/rotate",
            post(rotate_user_key),
        )
        .route(
            "/users/{user_id}/developer/keys/{key_id}/scopes",
            get(list_user_scopes).post(create_user_scope),
        )
        .route(
            "/users/{user_id}/developer/scopes/{scope_id}",
            delete(delete_user_scope),
        )
        .route("/developer-auth/audit", get(list_audit))
}

#[derive(Deserialize)]
struct AuditQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
struct CreateKeyRequest {
    name: String,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
struct IssuedKeyResponse {
    api_key: DeveloperApiKey,
    raw_key: String,
}

#[derive(Deserialize)]
struct CreateScopeRequest {
    match_type: ScopeMatchType,
    bucket: Option<String>,
    #[serde(default)]
    can_read: bool,
    #[serde(default)]
    can_write: bool,
}

#[derive(Serialize)]
struct ListAuditResponse {
    entries: Vec<DeveloperAuthAuditEntry>,
    total: i64,
    limit: i64,
    offset: i64,
}

async fn get_user_developer(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<DeveloperAccount>> {
    let (_, developer) = user_with_developer(&state, id).await?;
    Ok(Json(developer))
}

async fn list_user_keys(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<DeveloperApiKey>>> {
    let (_, developer) = user_with_developer(&state, id).await?;
    Ok(Json(
        state
            .developer_auth_repo
            .list_api_keys(developer.id)
            .await?,
    ))
}

async fn create_user_key(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(request): Json<CreateKeyRequest>,
) -> AppResult<(StatusCode, Json<IssuedKeyResponse>)> {
    let (user, _developer) = user_with_developer(&state, id).await?;
    ensure_user_developer_mutable(&user)?;
    let issued = state
        .developer_auth_use_cases
        .issue_api_key(user.id, &request.name, request.expires_at)
        .await?;
    write_audit(
        &state.developer_auth_repo,
        &admin,
        "developer_api_key.create",
        Some(user.id),
        Some(issued.api_key.id),
        json!({
            "key_name": issued.api_key.name,
            "key_prefix": issued.api_key.key_prefix,
        }),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(IssuedKeyResponse {
            api_key: issued.api_key,
            raw_key: issued.raw_key,
        }),
    ))
}

async fn revoke_user_key(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path((user_id, key_id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    let (user, _developer, key) = user_api_key(&state, user_id, key_id).await?;
    state.developer_auth_repo.revoke_api_key(key_id).await?;
    write_audit(
        &state.developer_auth_repo,
        &admin,
        "developer_api_key.revoke",
        Some(user.id),
        Some(key_id),
        json!({
            "key_name": key.name,
            "key_prefix": key.key_prefix,
        }),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn rotate_user_key(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path((user_id, key_id)): Path<(Uuid, Uuid)>,
) -> AppResult<(StatusCode, Json<IssuedKeyResponse>)> {
    let (user, _developer, _) = user_api_key(&state, user_id, key_id).await?;
    ensure_user_developer_mutable(&user)?;
    let issued = state
        .developer_auth_use_cases
        .rotate_api_key(key_id)
        .await?;
    write_audit(
        &state.developer_auth_repo,
        &admin,
        "developer_api_key.rotate",
        Some(user.id),
        Some(issued.api_key.id),
        json!({
            "rotated_from_api_key_id": key_id,
            "key_name": issued.api_key.name,
            "key_prefix": issued.api_key.key_prefix,
        }),
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(IssuedKeyResponse {
            api_key: issued.api_key,
            raw_key: issued.raw_key,
        }),
    ))
}

async fn list_user_scopes(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Path((user_id, key_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<Vec<DeveloperApiKeyScope>>> {
    let _ = user_api_key(&state, user_id, key_id).await?;
    Ok(Json(state.developer_auth_repo.list_scopes(key_id).await?))
}

async fn create_user_scope(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path((user_id, key_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<CreateScopeRequest>,
) -> AppResult<(StatusCode, Json<DeveloperApiKeyScope>)> {
    let (user, _developer, _) = user_api_key(&state, user_id, key_id).await?;
    ensure_user_developer_mutable(&user)?;
    validate_scope_request(&request)?;
    let scope = state
        .developer_auth_repo
        .create_scope(
            key_id,
            ScopeResourceType::Bucket,
            request.match_type,
            request.bucket.as_deref(),
            request.can_read,
            request.can_write,
        )
        .await?;
    write_audit(
        &state.developer_auth_repo,
        &admin,
        "developer_api_key_scope.create",
        Some(user.id),
        Some(key_id),
        json!({
            "scope_id": scope.id,
            "match_type": scope.match_type,
            "bucket": scope.resource_value,
            "can_read": scope.can_read,
            "can_write": scope.can_write,
        }),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(scope)))
}

async fn delete_user_scope(
    AdminUser(admin): AdminUser,
    State(state): State<AppState>,
    Path((user_id, scope_id)): Path<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    let (user, _developer, scope) = user_scope(&state, user_id, scope_id).await?;
    state.developer_auth_repo.delete_scope(scope_id).await?;
    write_audit(
        &state.developer_auth_repo,
        &admin,
        "developer_api_key_scope.delete",
        Some(user.id),
        Some(scope.api_key_id),
        json!({
            "scope_id": scope_id,
            "match_type": scope.match_type,
            "bucket": scope.resource_value,
        }),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_audit(
    AdminUser(_): AdminUser,
    State(state): State<AppState>,
    Query(query): Query<AuditQuery>,
) -> AppResult<Json<ListAuditResponse>> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let offset = query.offset.unwrap_or(0).max(0);
    let entries = state
        .developer_auth_repo
        .list_developer_auth_audit(limit, offset)
        .await?;
    let total = state
        .developer_auth_repo
        .count_developer_auth_audit()
        .await?;
    Ok(Json(ListAuditResponse {
        entries,
        total,
        limit,
        offset,
    }))
}

async fn user_with_developer(
    state: &AppState,
    user_id: Uuid,
) -> AppResult<(UserProfile, DeveloperAccount)> {
    let user = state
        .user_repo
        .get_profile_by_id(user_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let developer = state.developer_auth_use_cases.get_account(user.id).await?;
    Ok((user, developer))
}

async fn user_api_key(
    state: &AppState,
    user_id: Uuid,
    api_key_id: Uuid,
) -> AppResult<(UserProfile, DeveloperAccount, DeveloperApiKey)> {
    let (user, developer) = user_with_developer(state, user_id).await?;
    let key = state
        .developer_auth_repo
        .get_api_key(api_key_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if key.user_id != developer.id {
        return Err(AppError::NotFound);
    }
    Ok((user, developer, key))
}

async fn user_scope(
    state: &AppState,
    user_id: Uuid,
    scope_id: Uuid,
) -> AppResult<(UserProfile, DeveloperAccount, DeveloperApiKeyScope)> {
    let scope = state
        .developer_auth_repo
        .get_scope(scope_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let (user, developer, _) = user_api_key(state, user_id, scope.api_key_id).await?;
    Ok((user, developer, scope))
}

fn ensure_user_developer_mutable(user: &UserProfile) -> AppResult<()> {
    if user.is_frozen {
        return Err(AppError::Conflict("user account is frozen".into()));
    }
    Ok(())
}

fn validate_scope_request(request: &CreateScopeRequest) -> AppResult<()> {
    if !request.can_read && !request.can_write {
        return Err(AppError::InvalidInput(
            "scope must grant read or write".into(),
        ));
    }
    if request.match_type != ScopeMatchType::All
        && request
            .bucket
            .as_deref()
            .is_none_or(|bucket| bucket.trim().is_empty())
    {
        return Err(AppError::InvalidInput(
            "bucket is required for exact or prefix scopes".into(),
        ));
    }
    Ok(())
}

async fn write_audit(
    repo: &Arc<dyn DeveloperAuthRepo>,
    admin: &UserProfile,
    action: &str,
    target_user_id: Option<Uuid>,
    api_key_id: Option<Uuid>,
    metadata: serde_json::Value,
) -> AppResult<()> {
    repo.log_developer_auth_audit(NewDeveloperAuthAuditEntry {
        admin_id: admin.id,
        admin_email: admin.email.clone(),
        action: action.to_string(),
        target_user_id,
        api_key_id,
        metadata,
    })
    .await
}
